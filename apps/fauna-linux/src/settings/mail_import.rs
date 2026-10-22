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
//! - **Outlook** shows `source-username` (the Microsoft account's address, the
//!   XOAUTH2 user) and **Sign in with Microsoft** — no password field, no host
//!   field, no Connect button: Microsoft takes no password from a mail app, so
//!   the IMAP fallback is withdrawn (`mailbox-migration.md` § Microsoft
//!   sign-in — the device-code grant).
//! - **Generic** shows the IMAP fields alone; no app-password, no OAuth.
//!
//! # The Outlook source signs in with Microsoft
//!
//! `mail-import-source-oauth-button` dispatches the shared
//! `microsoft_sign_in_actions` (commit the address, `BeginMicrosoftSignIn`);
//! once Microsoft has answered with a code the dispatch spawns
//! `MailImportMachine::run_microsoft_sign_in`, the polling half, exactly as it
//! spawns `run_import`. While the code is out `mail-import-source-oauth-code`
//! paints the code and Microsoft's address (its `code` and `url` attributes
//! carry each alone) beside an id-less **Open in browser** that hands the
//! address to the OS opener; the grant connects by itself and the step moves
//! on to Scope with nothing more pressed. The Progress repaint tick also runs
//! while a sign-in is in flight, so the code appears without a click. A
//! platform without a registration paints the unavailable sentence and keeps
//! the button inert (`MailImportSnapshot::microsoft_signin_available`).
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
//! are not backed by any `MailImportAction`. View imported navigates to
//! Conversations (where mail lives). Review skipped is a page-local toggle
//! (`ImportCtx::show_skipped`, the tui `show_skipped` shape): it reveals the
//! snapshot's `error_log` — one line per skipped message or mailbox — below
//! the Done card (`mailbox-migration.md` § Wizard steps step 6).

use fauna_ui_ids as ids;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

use fauna_client_mail_settings::{
    DEFAULT_MAX_SIZE_MB, ImportPauseReason, ImportSessionState, ImportSourceKind, ImportStep,
    ImportTlsMode, MailImportAction, MailImportMachine, MailImportSnapshot, MicrosoftSignInState,
    SOURCE_KINDS, TLS_MODES, import_source_kind_label, import_tls_mode_label,
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

/// The Outlook source's Microsoft sign-in rows: the button, the code the grant
/// is waiting on beside Open in browser, and the sentence under them. Built
/// once for the Source step and once for an adopted session's Progress step,
/// which repaints the same ids (only the shown step's copy is findable).
#[derive(Clone)]
struct MicrosoftRows {
    row: adw::ActionRow,
    button: gtk::Button,
    code_row: adw::ActionRow,
    code_label: gtk::Label,
    open_button: gtk::Button,
    note: gtk::Label,
}

/// Build the sign-in rows into `group`, all hidden until a render shows them.
fn build_microsoft_rows(group: &adw::PreferencesGroup) -> MicrosoftRows {
    let button = gtk::Button::builder()
        .label(S::SOURCE_OAUTH_BUTTON)
        .valign(gtk::Align::Center)
        .build();
    set_test_id(&button, ids::MAIL_IMPORT_SOURCE_OAUTH_BUTTON);
    let row = adw::ActionRow::builder()
        .activatable(false)
        .visible(false)
        .build();
    row.add_suffix(&button);
    group.add(&row);

    let code_label = gtk::Label::builder().wrap(true).xalign(0.0).build();
    set_test_id(&code_label, ids::MAIL_IMPORT_SOURCE_OAUTH_CODE);
    let open_button = gtk::Button::builder()
        .label(S::MICROSOFT_OPEN_BROWSER)
        .valign(gtk::Align::Center)
        .build();
    let code_row = adw::ActionRow::builder()
        .activatable(false)
        .visible(false)
        .build();
    code_row.add_suffix(&code_label);
    code_row.add_suffix(&open_button);
    group.add(&code_row);

    let note = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .build();
    note.add_css_class("dim-label");
    group.add(&note);

    MicrosoftRows {
        row,
        button,
        code_row,
        code_label,
        open_button,
        note,
    }
}

/// Whether the Outlook source's sign-in is under way — a code requested, out,
/// or its grant connecting. The button stays inert meanwhile and the repaint
/// tick keeps running.
fn microsoft_sign_in_in_flight(snap: &MailImportSnapshot) -> bool {
    snap.microsoft_signin.as_ref().is_some_and(|v| {
        matches!(
            v.state,
            MicrosoftSignInState::Requesting
                | MicrosoftSignInState::Waiting
                | MicrosoftSignInState::Connecting
        )
    })
}

/// Paint one copy of the sign-in rows from `snap`; `shown` says whether the
/// step hosting them is the one asking for Microsoft.
fn render_microsoft_rows(m: &MicrosoftRows, snap: &MailImportSnapshot, shown: bool) {
    let available = snap.microsoft_signin_available;
    m.row.set_visible(shown);
    m.button
        .set_sensitive(available && !microsoft_sign_in_in_flight(snap));
    let waiting = snap.microsoft_signin.as_ref().filter(|v| {
        shown && available && v.state == MicrosoftSignInState::Waiting && !v.user_code.is_empty()
    });
    m.code_row.set_visible(waiting.is_some());
    if let Some(v) = waiting {
        m.code_label
            .set_text(&S::microsoft_code_fmt(&v.verification_uri, &v.user_code));
        // The code and the address each ride alone as attributes — the
        // observables the cross-app journey reads (tui's `.attr("code", …)`).
        crate::testid::set_test_attr_text(&m.code_label, "code", &v.user_code);
        crate::testid::set_test_attr_text(&m.code_label, "url", &v.verification_uri);
    }
    let note = if shown && !available {
        Some(S::MICROSOFT_UNAVAILABLE)
    } else if waiting.is_some() {
        Some(S::MICROSOFT_WAITING)
    } else {
        None
    };
    m.note.set_visible(note.is_some());
    if let Some(text) = note {
        m.note.set_label(text);
    }
}

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
    microsoft: MicrosoftRows,
    host_row: adw::ActionRow,
    host_input: gtk::Entry,
    port_row: adw::ActionRow,
    port_input: gtk::Entry,
    tls_mode_row: adw::ActionRow,
    tls_mode_picker: gtk::DropDown,
    password_row: adw::ActionRow,
    password_input: gtk::Entry,
    connect_row: adw::ActionRow,
    forget_row: adw::ActionRow,

    // Step 2 — Scope.
    mailboxes_group: adw::PreferencesGroup,
    mailbox_rows: Rc<std::cell::RefCell<Vec<gtk::CheckButton>>>,
    mailboxes_placeholder: adw::ActionRow,
    scope_group: adw::PreferencesGroup,
    date_from_input: gtk::Entry,
    max_size_input: gtk::Entry,
    import_duplicates_toggle: gtk::CheckButton,

    // Step 3 — Confirm.
    confirm_group: adw::PreferencesGroup,
    confirm_summary: gtk::Label,

    // Step 4 — Progress.
    progress_group: adw::PreferencesGroup,
    progress_summary: gtk::Label,
    progress_bar: gtk::ProgressBar,
    progress_controls_row: adw::ActionRow,
    restart_button: gtk::Button,
    dismiss_row: adw::ActionRow,
    // The credential a session adopted after a restart asks for beside Resume
    // (`MailImportSnapshot::resume_needs_credentials`, § Resume protocol step
    // 2): the Source step's own fields for the session's provider, repainted
    // on the Progress step under the same ids — only the mapped one answers.
    resume_notice: gtk::Label,
    // The full-mailbox notice and its Skip-the-rest button, shown while the
    // session is paused on `MailboxFull` (`mailbox-migration.md` § Failure
    // handling → *Nest-side errors*).
    quota_notice: gtk::Label,
    quota_skip_rest_button: gtk::Button,
    resume_app_password_row: adw::ActionRow,
    resume_app_password_input: gtk::Entry,
    resume_microsoft: MicrosoftRows,
    resume_port_row: adw::ActionRow,
    resume_port_input: gtk::Entry,
    resume_tls_mode_row: adw::ActionRow,
    resume_tls_mode_picker: gtk::DropDown,
    resume_password_row: adw::ActionRow,
    resume_password_input: gtk::Entry,
    error_log: gtk::Label,
    mailbox_progress_group: adw::PreferencesGroup,
    mailbox_progress_rows: Rc<std::cell::RefCell<Vec<adw::ActionRow>>>,

    // Step 5 — Done.
    done_group: adw::PreferencesGroup,
    done_summary: gtk::Label,
    /// The skip log Review skipped reveals below the Done card — a second
    /// `mail-import-error-log` label (the Progress step's own is hidden by
    /// then, and `automation::find` prunes non-showing subtrees).
    done_error_log: gtk::Label,
    done_error_log_row: adw::ActionRow,
}

/// Everything the page's handlers + render need. `Rc`-shared into every closure.
struct ImportCtx {
    machine: Arc<MailImportMachine>,
    rt: tokio::runtime::Handle,
    /// Set while `render()` programmatically updates a picker, so its
    /// `selected_notify` handler doesn't echo the change back as a dispatch.
    syncing: Cell<bool>,
    /// Whether Review skipped has revealed the skip log on the Done screen —
    /// page-local, the tui `MailImportState::show_skipped` shape; cleared by
    /// `render()` whenever the wizard is off the Done step.
    show_skipped: Cell<bool>,
    /// Whether the Progress repaint tick is already running — one at a time.
    ticking: Cell<bool>,
    /// Whether the last rendered snapshot asked for the resume credential —
    /// the edge that seeds the port draft and the one that drops the password.
    resume_asked: Cell<bool>,
    /// Whether this visit's Source drafts were filled from the remembered
    /// source already — once per visit, so a repaint never refills a field the
    /// user emptied (the tui `MailImportState::drafts_seeded` shape).
    drafts_seeded: Cell<bool>,
    /// Whether the last snapshot seen had a remembered source — the edge that
    /// empties the drafts when a Forget lands.
    was_remembered: Cell<bool>,
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
    if let Some(ctx) = wire_machine(widgets, buttons) {
        // The page already open when another device starts, pauses, fails or
        // dismisses an import (`StaleSurfaces::mail_import`,
        // `mailbox-migration.md` § A failed import is a record the wizard
        // shows until the user moves on): page-gated on GTK's map state, the
        // export page's shape — a run's progress is one push per batch. The
        // machine's `Refresh` ignores the nudge while it holds a run.
        let weak = page.downgrade();
        crate::settings::set_mail_import_rehydrate_handler(std::rc::Rc::new(move || {
            if weak.upgrade().is_some_and(|p| p.is_mapped()) {
                hydrate_and_render(&ctx);
            }
        }));
    }
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

    // Outlook — the Microsoft sign-in (module docs).
    let microsoft = build_microsoft_rows(&source_group);

    // Generic.
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

    // The remembered source's forget control, for every provider while the
    // account remembers one (`MailImportSnapshot::source_remembered`).
    let forget_button = wizard_button(
        ids::MAIL_IMPORT_SOURCE_FORGET_BUTTON,
        S::SOURCE_FORGET_BUTTON,
    );
    let forget_row = adw::ActionRow::builder()
        .activatable(false)
        .visible(false)
        .build();
    forget_row.add_suffix(&forget_button);
    source_group.add(&forget_row);

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

    // The advanced toggle; its label carries the double-storage warning itself.
    let import_duplicates_toggle = gtk::CheckButton::builder()
        .label(S::SCOPE_IMPORT_DUPLICATES_LABEL)
        .build();
    set_test_id(
        &import_duplicates_toggle,
        ids::MAIL_IMPORT_SCOPE_IMPORT_DUPLICATES_TOGGLE,
    );
    mark_row_state(&import_duplicates_toggle, false);
    let import_duplicates_row = adw::ActionRow::builder().activatable(false).build();
    import_duplicates_row.add_prefix(&import_duplicates_toggle);
    scope_group.add(&import_duplicates_row);

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

    let resume_notice = gtk::Label::builder()
        .label(S::RESUME_NEEDS_PASSWORD)
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .build();
    resume_notice.add_css_class("dim-label");
    progress_group.add(&resume_notice);
    let quota_notice = gtk::Label::builder()
        .label(S::QUOTA_NOTICE)
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .build();
    quota_notice.add_css_class("dim-label");
    set_test_id(&quota_notice, ids::MAIL_IMPORT_QUOTA_NOTICE);
    progress_group.add(&quota_notice);
    let (resume_app_password_row, resume_app_password_input) = entry_row(
        ids::MAIL_IMPORT_SOURCE_APP_PASSWORD,
        S::SOURCE_APP_PASSWORD_LABEL,
        S::SOURCE_APP_PASSWORD_LABEL,
    );
    resume_app_password_input.set_visibility(false);
    resume_app_password_row.set_visible(false);
    progress_group.add(&resume_app_password_row);
    let resume_microsoft = build_microsoft_rows(&progress_group);
    let (resume_port_row, resume_port_input) = entry_row(
        ids::MAIL_IMPORT_SOURCE_PORT,
        S::SOURCE_PORT_PLACEHOLDER,
        S::SOURCE_PORT_PLACEHOLDER,
    );
    resume_port_row.set_visible(false);
    progress_group.add(&resume_port_row);
    let resume_tls_options: Vec<String> = TLS_MODES.into_iter().map(tls_mode_label).collect();
    let resume_tls_refs: Vec<&str> = resume_tls_options.iter().map(String::as_str).collect();
    let resume_tls_mode_picker = gtk::DropDown::from_strings(&resume_tls_refs);
    resume_tls_mode_picker.set_valign(gtk::Align::Center);
    set_test_id(&resume_tls_mode_picker, ids::MAIL_IMPORT_SOURCE_TLS_MODE);
    let resume_tls_mode_row = adw::ActionRow::builder()
        .title(S::TLS_IMPLICIT)
        .visible(false)
        .build();
    resume_tls_mode_row.add_suffix(&resume_tls_mode_picker);
    progress_group.add(&resume_tls_mode_row);
    let (resume_password_row, resume_password_input) = entry_row(
        ids::MAIL_IMPORT_SOURCE_PASSWORD,
        S::SOURCE_PASSWORD_PLACEHOLDER,
        S::SOURCE_PASSWORD_PLACEHOLDER,
    );
    resume_password_input.set_visibility(false);
    resume_password_row.set_visible(false);
    progress_group.add(&resume_password_row);

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
    // Offered only while the shared machine holds a source mailbox the old
    // provider rebuilt (`MailImportSnapshot::rebuilt_mailboxes`).
    let restart_button = gtk::Button::builder()
        .label(S::RESTART_BUTTON)
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    set_test_id(&restart_button, ids::MAIL_IMPORT_RESTART_BUTTON);
    let cancel_button = gtk::Button::builder()
        .label(S::CANCEL_BUTTON)
        .valign(gtk::Align::Center)
        .css_classes(["destructive-action"])
        .build();
    set_test_id(&cancel_button, ids::MAIL_IMPORT_CANCEL_BUTTON);
    let progress_controls_row = adw::ActionRow::builder().activatable(false).build();
    progress_controls_row.add_suffix(&pause_button);
    progress_controls_row.add_suffix(&resume_button);
    let quota_skip_rest_button = gtk::Button::builder()
        .label(S::QUOTA_SKIP_REST_BUTTON)
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    set_test_id(
        &quota_skip_rest_button,
        ids::MAIL_IMPORT_QUOTA_SKIP_REST_BUTTON,
    );
    progress_controls_row.add_suffix(&quota_skip_rest_button);
    progress_controls_row.add_suffix(&restart_button);
    progress_controls_row.add_suffix(&cancel_button);
    progress_group.add(&progress_controls_row);
    // A failed import is a record, not a run (`mailbox-migration.md` § A
    // failed import is a record the wizard shows until the user moves on): the
    // controls above give way to this one.
    let dismiss_button = gtk::Button::builder()
        .label(S::DISMISS_FAILED_BUTTON)
        .valign(gtk::Align::Center)
        .build();
    set_test_id(&dismiss_button, ids::MAIL_IMPORT_DISMISS_BUTTON);
    let dismiss_row = adw::ActionRow::builder()
        .activatable(false)
        .visible(false)
        .build();
    dismiss_row.add_suffix(&dismiss_button);
    progress_group.add(&dismiss_row);

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
    let done_error_log = super::blank_value_marker(ids::MAIL_IMPORT_ERROR_LOG);
    let done_error_log_row = adw::ActionRow::builder()
        .title(S::ERROR_LOG_TITLE)
        .visible(false)
        .build();
    done_error_log_row.add_suffix(&done_error_log);
    done_group.add(&done_error_log_row);
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
        microsoft,
        host_row,
        host_input,
        port_row,
        port_input,
        tls_mode_row,
        tls_mode_picker,
        password_row,
        password_input,
        connect_row,
        forget_row,
        mailboxes_group,
        mailbox_rows: Rc::new(std::cell::RefCell::new(Vec::new())),
        mailboxes_placeholder,
        scope_group,
        date_from_input,
        max_size_input,
        import_duplicates_toggle,
        confirm_group,
        confirm_summary,
        progress_group,
        progress_summary,
        progress_bar,
        progress_controls_row,
        restart_button,
        dismiss_row,
        resume_notice,
        quota_notice,
        quota_skip_rest_button,
        resume_app_password_row,
        resume_app_password_input,
        resume_microsoft,
        resume_port_row,
        resume_port_input,
        resume_tls_mode_row,
        resume_tls_mode_picker,
        resume_password_row,
        resume_password_input,
        error_log,
        mailbox_progress_group,
        mailbox_progress_rows: Rc::new(std::cell::RefCell::new(Vec::new())),
        done_group,
        done_summary,
        done_error_log,
        done_error_log_row,
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
        forget_button,
        dismiss_button,
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
    forget_button: gtk::Button,
    dismiss_button: gtk::Button,
    view_imported_button: gtk::Button,
    review_skipped_button: gtk::Button,
}

/// Connect the page to the shared `MailImportMachine`, hydrate on mount, and
/// wire every interaction. Returns the page's context, for the push re-read;
/// `None` (and a no-op) when no client is available (the unit test).
fn wire_machine(widgets: ImportWidgets, b: WizardButtons) -> Option<Rc<ImportCtx>> {
    let client = crate::settings::get_client()?;
    let machine = Arc::new(
        crate::mail_glue::build_mail_import_machine(&client)
            .with_source_memory(crate::account_runtime::mail_import_source_store()),
    );

    let ctx = Rc::new(ImportCtx {
        machine,
        rt: client.runtime_handle(),
        syncing: Cell::new(false),
        show_skipped: Cell::new(false),
        ticking: Cell::new(false),
        resume_asked: Cell::new(false),
        drafts_seeded: Cell::new(false),
        was_remembered: Cell::new(false),
        w: widgets,
    });

    hydrate_and_render(&ctx);

    // Re-read whenever the page is shown again: the sub-page outlives its first
    // visit, and the deployment's Microsoft registration (read off
    // `fauna.setup.status` at every refresh, `microsoft_signin_available`) is set
    // by an admin on `admin-mail` after this page was built.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.source_picker.clone().connect_map(move |_| {
            ctx.drafts_seeded.set(false);
            hydrate_and_render(&ctx);
        });
    }

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
    // dispatched live so the preset/override distinction stays visible. The
    // Progress step's resume picker is the same choice for an adopted session.
    for picker in [&ctx.w.tls_mode_picker, &ctx.w.resume_tls_mode_picker] {
        let ctx = Rc::clone(&ctx);
        picker.clone().connect_selected_notify(move |dd| {
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
    // Sign in with Microsoft — on the Source step and on an adopted session's
    // Progress step alike (the same ids, one findable at a time).
    for button in [&ctx.w.microsoft.button, &ctx.w.resume_microsoft.button] {
        let ctx = Rc::clone(&ctx);
        button.connect_clicked(move |_| dispatch_actions(&ctx, microsoft_start_actions(&ctx)));
    }
    // Open in browser hands the sign-in address to the OS opener — purely
    // local, and silent under the e2e harness (`url_opener`).
    for button in [
        &ctx.w.microsoft.open_button,
        &ctx.w.resume_microsoft.open_button,
    ] {
        let ctx = Rc::clone(&ctx);
        button.connect_clicked(move |_| {
            let snap = ctx.machine.snapshot();
            if let Some(view) = snap
                .microsoft_signin
                .as_ref()
                .filter(|v| !v.verification_uri.is_empty())
            {
                crate::url_opener::open(&view.verification_uri);
            }
        });
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
    wire_dispatch(
        &ctx,
        &ctx.w.restart_button,
        MailImportAction::RestartRebuiltMailboxes,
    );
    wire_dispatch(
        &ctx,
        &ctx.w.quota_skip_rest_button,
        MailImportAction::SkipRest,
    );
    wire_dispatch(&ctx, &b.forget_button, MailImportAction::ForgetSource);
    wire_dispatch(&ctx, &b.dismiss_button, MailImportAction::Dismiss);
    // The import-duplicates toggle dispatches the new value. The state marker is
    // re-applied SYNCHRONOUSLY for the reason spelled out on the mailbox rows.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .import_duplicates_toggle
            .clone()
            .connect_toggled(move |cb| {
                if ctx.syncing.get() {
                    return;
                }
                mark_row_state(cb, cb.is_active());
                dispatch_actions(
                    &ctx,
                    vec![MailImportAction::SetImportDuplicates {
                        value: cb.is_active(),
                    }],
                );
            });
    }
    // Step 5's Resume — commits the resume credential first when the session
    // was adopted after a restart (shared `resume_actions`).
    {
        let ctx = Rc::clone(&ctx);
        b.resume_button
            .connect_clicked(move |_| dispatch_actions(&ctx, resume_actions(&ctx)));
    }

    // View imported has no machine action — it goes to Conversations, where
    // mail lives (module docs).
    b.view_imported_button.connect_clicked(|btn| {
        crate::app::navigate_to_section(btn, "conversations");
    });
    // Review skipped is a view toggle: the skip log is already on the
    // snapshot, so no machine action — flip the flag and repaint.
    {
        let ctx = Rc::clone(&ctx);
        b.review_skipped_button.connect_clicked(move |_| {
            ctx.show_skipped.set(!ctx.show_skipped.get());
            render(&ctx, &ctx.machine.snapshot());
        });
    }

    // Cancel is destructive — two-click inline confirm (already-imported
    // messages are kept, § UX shape step 5).
    let cancel_ctx = Rc::clone(&ctx);
    super::wire_two_click(
        &b.cancel_button,
        S::CANCEL_BUTTON,
        crate::i18n::strings::common::CONFIRM_Q,
        true,
        |_| {},
        |_| {},
        move || dispatch_actions(&cancel_ctx, vec![MailImportAction::Cancel]),
    );
    Some(ctx)
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

/// **Sign in with Microsoft**: commit the address the field holds, then ask
/// for a code (the shared `microsoft_sign_in_actions`); on an adopted session's
/// Progress step the address is the session's own, so a bare
/// `BeginMicrosoftSignIn`.
fn microsoft_start_actions(ctx: &Rc<ImportCtx>) -> Vec<MailImportAction> {
    if ctx.machine.snapshot().resume_needs_credentials {
        vec![MailImportAction::BeginMicrosoftSignIn]
    } else {
        fauna_client_mail_settings::microsoft_sign_in_actions(&ctx.w.username_input.text())
    }
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

/// Step 5's Resume: on a session adopted after a restart, commit the
/// credential the Progress step painted first. The wire-action shape is
/// `fauna_client_mail_settings::resume_actions`; this side's own job is
/// locating the strings — the password sits in the app-password entry for
/// Gmail / iCloud and the password entry otherwise, as for [`connect_actions`].
fn resume_actions(ctx: &Rc<ImportCtx>) -> Vec<MailImportAction> {
    let w = &ctx.w;
    let snap = ctx.machine.snapshot();
    let secret = match snap.source_kind {
        ImportSourceKind::Gmail | ImportSourceKind::ICloud => w.resume_app_password_input.text(),
        _ => w.resume_password_input.text(),
    };
    fauna_client_mail_settings::resume_actions(
        snap.resume_needs_credentials,
        snap.source_kind,
        &w.resume_port_input.text(),
        SecretString::from(secret.to_string()),
    )
}

/// Paint the resume credential for an adopted session
/// (`MailImportSnapshot::resume_needs_credentials`): the sentence saying why
/// and the session provider's own field — the app password, Generic's port,
/// connection security and password, or the Microsoft sign-in. Seeds the port
/// draft from the session when the ask begins and drops the typed password
/// once a Resume has used it.
fn render_resume_credential(ctx: &Rc<ImportCtx>, snap: &MailImportSnapshot) {
    let w = &ctx.w;
    let asked = snap.resume_needs_credentials
        && snap.step == ImportStep::Progress
        && snap.session_state != Some(ImportSessionState::Errored);
    if asked && !ctx.resume_asked.get() {
        w.resume_port_input.set_text(&snap.port.to_string());
    }
    if !asked && ctx.resume_asked.get() {
        w.resume_password_input.set_text("");
        w.resume_app_password_input.set_text("");
    }
    ctx.resume_asked.set(asked);
    let kind = snap.source_kind;
    let app_password_kind = matches!(kind, ImportSourceKind::Gmail | ImportSourceKind::ICloud);
    let outlook = kind == ImportSourceKind::Outlook;
    let generic = kind == ImportSourceKind::Generic;
    w.resume_notice.set_visible(asked);
    w.resume_notice.set_label(if outlook {
        S::RESUME_NEEDS_MICROSOFT
    } else {
        S::RESUME_NEEDS_PASSWORD
    });
    w.resume_app_password_row
        .set_visible(asked && app_password_kind);
    render_microsoft_rows(&w.resume_microsoft, snap, asked && outlook);
    w.resume_port_row.set_visible(asked && generic);
    w.resume_tls_mode_row.set_visible(asked && generic);
    w.resume_password_row.set_visible(asked && generic);
}

/// A failed import paints Dismiss as the Progress step's one control and none of
/// Pause / Resume / Restart / Cancel (`mailbox-migration.md` § A failed import is
/// a record the wizard shows until the user moves on); a live one keeps the
/// controls, with the restart offered only while a source mailbox the old
/// provider rebuilt is pending (§ Resume protocol step 4).
fn apply_progress_controls(
    controls_row: &adw::ActionRow,
    restart_button: &gtk::Button,
    dismiss_row: &adw::ActionRow,
    snap: &MailImportSnapshot,
) {
    let failed = snap.session_state == Some(ImportSessionState::Errored);
    controls_row.set_visible(!failed);
    dismiss_row.set_visible(failed);
    restart_button.set_visible(!snap.rebuilt_mailboxes.is_empty());
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
        move |snap| after_snapshot(&ctx_render, &snap, false, false, false),
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

/// The source the account remembers (`mailbox-migration.md` § Credential
/// handling): the machine prefilled the Source step, so fill this visit's
/// drafts from it once — never over what the user has typed — and empty them
/// when the user's Forget lands (tui: `settings::apply_mail_import_snapshot`).
fn sync_remembered_drafts(
    w: &ImportWidgets,
    seeded: &Cell<bool>,
    was_remembered: &Cell<bool>,
    snap: &MailImportSnapshot,
) {
    if snap.step == ImportStep::Source {
        if snap.source_remembered && !seeded.get() && w.username_input.text().is_empty() {
            w.host_input.set_text(&snap.host);
            w.port_input.set_text(&snap.port.to_string());
            w.username_input.set_text(&snap.username);
            seeded.set(true);
        } else if was_remembered.get() && !snap.source_remembered {
            for entry in [
                &w.host_input,
                &w.port_input,
                &w.username_input,
                &w.password_input,
                &w.app_password_input,
            ] {
                entry.set_text("");
            }
        }
    }
    was_remembered.set(snap.source_remembered);
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
        .any(|a| fauna_client_mail_settings::import_action_starts_run(a.clone()));
    // Same introspection as `should_run`: a `SelectSourceKind` dispatch's
    // post-dispatch snapshot is what re-seeds the Source-step host/port
    // drafts (`seed_source_drafts`'s doc comment) — never the general render
    // fold, which every other dispatch and the progress tick also go through.
    let kind_changed = actions
        .iter()
        .any(|a| matches!(a, MailImportAction::SelectSourceKind { .. }));
    let began_sign_in = actions
        .iter()
        .any(|a| matches!(a, MailImportAction::BeginMicrosoftSignIn));
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            for action in actions {
                let _ = machine.dispatch(action).await;
            }
            machine.snapshot()
        },
        move |snap| after_snapshot(&ctx_render, &snap, should_run, kind_changed, began_sign_in),
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
    began_sign_in: bool,
) {
    if kind_changed {
        seed_source_drafts(&ctx.w, snap);
    }
    sync_remembered_drafts(&ctx.w, &ctx.drafts_seeded, &ctx.was_remembered, snap);
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
    // The Microsoft sign-in's polling half, once Microsoft answered with a
    // code — never on a refused `BeginMicrosoftSignIn` (unavailable, no
    // address). Fire-and-forget like `run_import`: the tick re-reads the
    // snapshot while the sign-in is in flight, and a grant connects by itself.
    if began_sign_in
        && snap
            .microsoft_signin
            .as_ref()
            .is_some_and(|v| v.state == MicrosoftSignInState::Waiting)
    {
        let machine = Arc::clone(&ctx.machine);
        ctx.rt.spawn(async move {
            if let Err(e) = machine.run_microsoft_sign_in().await {
                tracing::warn!("[settings/mail_import] run_microsoft_sign_in: {e:?}");
            }
        });
        start_progress_tick(ctx);
    }
}

/// Repaint the page from the machine while `run_import` runs — and, on the
/// Source step, while a Microsoft sign-in is in flight.
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
    let generation = crate::actor_scope::current_generation();
    glib::timeout_add_local(
        std::time::Duration::from_millis(u64::from(PROGRESS_TICK_MS)),
        move || {
            let snap = ctx.machine.snapshot();
            let (flow, painted) = progress_tick_fire(
                crate::actor_scope::poll_still_current,
                generation,
                &ctx.ticking,
                snap.step,
                microsoft_sign_in_in_flight(&snap),
            );
            if painted {
                render(&ctx, &snap);
            }
            flow
        },
    );
}

/// One fire of the Progress tick, minus the GTK work (the export twin's shape):
/// the closure reads the snapshot, calls this, and paints only when it answers
/// `painted`. The generation check is a parameter so a test drives it with a
/// local counter, not the process-global one.
///
/// Retired by the canonical drop: a Paused/Errored session stays on Progress, so
/// the step test alone would keep this tick (and the outgoing account's machine)
/// alive across a switch.
fn progress_tick_fire(
    still_current: impl FnOnce(u64) -> bool,
    generation: u64,
    ticking: &Cell<bool>,
    step: ImportStep,
    signing_in: bool,
) -> (glib::ControlFlow, bool) {
    if !still_current(generation) {
        ticking.set(false);
        return (glib::ControlFlow::Break, false);
    }
    if step == ImportStep::Progress || signing_in {
        (glib::ControlFlow::Continue, true)
    } else {
        ticking.set(false);
        (glib::ControlFlow::Break, true)
    }
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
    let outlook = kind == ImportSourceKind::Outlook;
    let imap_fields_kind = kind == ImportSourceKind::Generic;
    // Painted for every kind — nothing in ui.yaml scopes `source-username` to a
    // subset the way the other four are annotated. Outlook asks for the
    // Microsoft account's address, the XOAUTH2 user.
    w.username_row.set_visible(true);
    let username_label = if outlook {
        S::SOURCE_OUTLOOK_ADDRESS_LABEL
    } else {
        S::SOURCE_USERNAME_PLACEHOLDER
    };
    w.username_row.set_title(username_label);
    w.username_input.set_placeholder_text(Some(username_label));
    w.app_password_row.set_visible(app_password_kind);
    w.app_password_help.set_visible(app_password_kind);
    let help = if kind == ImportSourceKind::Gmail {
        S::SOURCE_APP_PASSWORD_HELP_GMAIL
    } else {
        S::SOURCE_APP_PASSWORD_HELP_ICLOUD
    };
    w.app_password_help.set_label(help);
    // The explanation also rides the field as its `help` attribute — the
    // observable the cross-app journey reads (tui's `.attr("help", …)`).
    crate::testid::set_test_attr_text(&w.app_password_input, "help", help);
    w.host_row.set_visible(imap_fields_kind);
    w.port_row.set_visible(imap_fields_kind);
    w.tls_mode_row.set_visible(imap_fields_kind);
    w.password_row.set_visible(imap_fields_kind);
    // The Microsoft grant connects by itself — no Connect button beside it.
    w.connect_row.set_visible(!outlook);
    // The sign-in button is revealed here, synchronously, for the same reason
    // as the fields above; `render()` then paints its state from the snapshot.
    w.microsoft.row.set_visible(outlook);
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
    if snap.step != ImportStep::Done {
        ctx.show_skipped.set(false);
    }

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
    if w.resume_tls_mode_picker.selected() != tls_mode_index(snap.tls_mode) {
        w.resume_tls_mode_picker
            .set_selected(tls_mode_index(snap.tls_mode));
    }
    ctx.syncing.set(false);

    apply_source_visibility(w, snap.source_kind);
    w.forget_row.set_visible(snap.source_remembered);
    if w.import_duplicates_toggle.is_active() != snap.import_duplicates {
        ctx.syncing.set(true);
        w.import_duplicates_toggle
            .set_active(snap.import_duplicates);
        ctx.syncing.set(false);
    }
    mark_row_state(&w.import_duplicates_toggle, snap.import_duplicates);
    render_microsoft_rows(
        &w.microsoft,
        snap,
        snap.source_kind == ImportSourceKind::Outlook,
    );

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
    apply_progress_controls(
        &w.progress_controls_row,
        &w.restart_button,
        &w.dismiss_row,
        snap,
    );
    render_resume_credential(ctx, snap);
    w.progress_summary.set_text(&progress_text(snap));
    let frac =
        fauna_core::format::quota_fraction(snap.imported_count as i64, snap.total_count as i64);
    w.progress_bar.set_fraction(frac);
    // The bar's filled fraction as the `fraction` attribute every app serves
    // (the bar's own text is a per-app rendering).
    crate::testid::set_test_attr_text(&w.progress_bar, "fraction", &format!("{frac:.4}"));
    // The full-mailbox notice: ask the admin then Resume, Skip the rest or
    // Cancel (the same condition tui paints it on).
    let mailbox_full = snap.pause_reason == Some(ImportPauseReason::MailboxFull)
        && snap.session_state == Some(ImportSessionState::Paused);
    w.quota_notice.set_visible(mailbox_full);
    w.quota_skip_rest_button.set_visible(mailbox_full);
    w.error_log.set_text(&snap.error_log.join("\n"));
    {
        let mut rows = w.mailbox_progress_rows.borrow_mut();
        for row in rows.drain(..) {
            w.mailbox_progress_group.remove(&row);
        }
        // One row per mailbox the run takes (laid at Start), each with its own
        // counts from the shared machine's `mailbox_progress`.
        for mb in &snap.mailbox_progress {
            let row = adw::ActionRow::builder().title(&mb.name).build();
            row.add_prefix(&super::marker(ids::MAIL_IMPORT_MAILBOX_PROGRESS_LIST_ITEM));
            let name_marker =
                super::blank_value_marker(ids::MAIL_IMPORT_MAILBOX_PROGRESS_LIST_ITEM_NAME);
            name_marker.set_text(&mb.name);
            row.add_suffix(&name_marker);
            let prog_marker =
                super::blank_value_marker(ids::MAIL_IMPORT_MAILBOX_PROGRESS_LIST_ITEM_PROGRESS);
            prog_marker.set_text(
                &fauna_client_mail_settings::import_mailbox_progress_text(mb.clone())
                    .resolve(crate::i18n::strings::lookup),
            );
            row.add_suffix(&prog_marker);
            w.mailbox_progress_group.add(&row);
            rows.push(row);
        }
    }

    // Done fields.
    w.done_summary.set_text(&done_text(snap));
    let reveal = ctx.show_skipped.get() && snap.step == ImportStep::Done;
    w.done_error_log_row.set_visible(reveal);
    w.done_error_log.set_text(&snap.error_log.join("\n"));
}

/// The Confirm summary: the source, the selected-mailbox count, their message
/// total and — when every one of them was sized — roughly how much data
/// (`mailbox-migration.md` § Wizard steps step 4), all from the shared
/// `MailImportSnapshot::selected_totals`.
fn confirm_text(snap: &MailImportSnapshot) -> String {
    let totals = snap.selected_totals();
    let source = source_kind_label(snap.source_kind);
    let mailboxes = totals.mailboxes.to_string();
    let messages = totals.messages.to_string();
    match totals.bytes {
        Some(bytes) => S::confirm_summary_sized_fmt(
            &source,
            &mailboxes,
            &messages,
            &crate::i18n::byte_size(bytes),
        ),
        None => S::confirm_summary_fmt(&source, &mailboxes, &messages),
    }
}

/// The progress summary with the run's elapsed time and, while it is
/// running, the time left (`mailbox-migration.md` § Wizard steps step 5; the
/// shared `transfer_timing` decides both).
fn progress_text(snap: &MailImportSnapshot) -> String {
    let summary = S::progress_summary_fmt(
        &snap.imported_count.to_string(),
        &snap.total_count.to_string(),
        &snap.skipped_count.to_string(),
        &snap.errored_count.to_string(),
    );
    let t = snap.timing(fauna_core::data::Timestamp::now_secs());
    match (t.elapsed_secs, t.remaining_secs) {
        (Some(elapsed), Some(remaining)) => {
            S::progress_remaining_fmt(&summary, &duration_text(elapsed), &duration_text(remaining))
        }
        (Some(elapsed), None) => S::progress_elapsed_fmt(&summary, &duration_text(elapsed)),
        _ => summary,
    }
}

/// A duration in the shared coarse format (`value-formatting.md` § Duration).
fn duration_text(secs: u64) -> String {
    fauna_core::format::duration_secs(secs).resolve(crate::i18n::strings::lookup)
}

/// § Wizard steps step 6: the counts and the time elapsed; a snapshot with
/// no clock yet keeps the untimed form.
fn done_text(snap: &MailImportSnapshot) -> String {
    let imported = snap.imported_count.to_string();
    let skipped = snap.skipped_count.to_string();
    let errored = snap.errored_count.to_string();
    match snap
        .timing(fauna_core::data::Timestamp::now_secs())
        .elapsed_secs
    {
        Some(elapsed) => {
            S::done_summary_timed_fmt(&imported, &skipped, &errored, &duration_text(elapsed))
        }
        None => S::done_summary_fmt(&imported, &skipped, &errored),
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// Source-shape pin of a wizard's `start_progress_tick` arm site, shared by the
    /// export and import test modules: the glib timeout closure cannot be built
    /// without a GTK main loop, so the mechanism is asserted on the source
    /// (`review-method.md` § A guard whose failure is silent lives inside the
    /// consumer). Whitespace is stripped so the match anchors on identifiers.
    /// The generation is captured at arm time, before the closure, and the fire
    /// gets the real `poll_still_current` with that captured generation —
    /// `|_| true`, or a per-fire re-read of the generation, never sees a drop.
    pub(crate) fn assert_progress_tick_arm_site(src: &str, wizard: &str) {
        let after = src
            .split_once("fn start_progress_tick(")
            .unwrap_or_else(|| panic!("{wizard}: `start_progress_tick` is in the file"))
            .1;
        let end = after
            .find("\n}\n")
            .unwrap_or_else(|| panic!("{wizard}: `start_progress_tick` ends at a column-0 brace"));
        let body: String = after[..end]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(
            body.contains("progress_tick_fire("),
            "{wizard}: vacuity guard — the arm site calls `progress_tick_fire(`"
        );
        let captured = body
            .find("letgeneration=crate::actor_scope::current_generation();")
            .unwrap_or_else(|| {
                panic!("{wizard}: the arm site captures `current_generation()` into `generation`")
            });
        let arm = body
            .find("glib::timeout_add_local(")
            .unwrap_or_else(|| panic!("{wizard}: the arm site arms a glib timeout"));
        assert!(
            captured < arm,
            "{wizard}: the generation must be captured at arm time, before the closure — a \
             per-fire re-read never sees the canonical drop"
        );
        assert!(
            body.contains("progress_tick_fire(crate::actor_scope::poll_still_current,generation,"),
            "{wizard}: the fire must get the real `poll_still_current` and the arm-time \
             `generation` as its first two arguments"
        );
    }

    #[test]
    fn the_progress_tick_arm_site_captures_the_generation_and_passes_the_real_check() {
        assert_progress_tick_arm_site(include_str!("mail_import.rs"), "import");
    }

    /// Behavioural pin of the tick's canonical-drop retirement through the
    /// lifted per-fire body, on a LOCAL generation counter: a live generation
    /// keeps ticking and paints; after the drop a tick still on Progress (the
    /// Paused/Errored case) breaks, clears `ticking` and paints nothing.
    #[test]
    fn the_progress_tick_fire_retires_on_the_canonical_drop() {
        use crate::actor_scope::{bump_actor_generation_in, poll_still_current_in};
        use std::sync::atomic::AtomicU64;
        let counter = AtomicU64::new(0);
        let armed = 0;
        let ticking = Cell::new(true);
        let (flow, painted) = progress_tick_fire(
            |g| poll_still_current_in(&counter, g),
            armed,
            &ticking,
            ImportStep::Progress,
            false,
        );
        assert_eq!(flow, glib::ControlFlow::Continue, "live generation");
        assert!(
            painted && ticking.get(),
            "live generation paints, keeps ticking"
        );

        bump_actor_generation_in(&counter);
        let (flow, painted) = progress_tick_fire(
            |g| poll_still_current_in(&counter, g),
            armed,
            &ticking,
            ImportStep::Progress,
            false,
        );
        assert_eq!(flow, glib::ControlFlow::Break, "dropped generation");
        assert!(!painted, "a retired tick paints nothing");
        assert!(!ticking.get(), "a retired tick clears ticking");
    }

    /// A Microsoft sign-in in flight keeps the tick running on the Source step,
    /// where the code appears with no click; once it is over the tick paints
    /// the step it landed on and retires.
    #[test]
    fn the_progress_tick_runs_while_a_microsoft_sign_in_is_in_flight() {
        use crate::actor_scope::poll_still_current_in;
        use std::sync::atomic::AtomicU64;
        let counter = AtomicU64::new(0);
        let ticking = Cell::new(true);
        let (flow, painted) = progress_tick_fire(
            |g| poll_still_current_in(&counter, g),
            0,
            &ticking,
            ImportStep::Source,
            true,
        );
        assert_eq!(flow, glib::ControlFlow::Continue, "signing in on Source");
        assert!(painted && ticking.get());

        let (flow, painted) = progress_tick_fire(
            |g| poll_still_current_in(&counter, g),
            0,
            &ticking,
            ImportStep::Scope,
            false,
        );
        assert_eq!(
            flow,
            glib::ControlFlow::Break,
            "the grant moved on to Scope"
        );
        assert!(painted, "the landing step is painted once");
        assert!(!ticking.get());
    }

    /// The sign-in rows follow the snapshot: inert beside the unavailable
    /// sentence, the code row only while Microsoft's code is out, carrying the
    /// code and the address as attributes.
    #[test]
    fn the_microsoft_rows_follow_the_snapshot() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let (_page, w, _buttons) = build_page_parts();
            let mut snap = a_mail_import_snapshot(ImportStep::Source);
            snap.source_kind = ImportSourceKind::Outlook;

            snap.microsoft_signin_available = false;
            render_microsoft_rows(&w.microsoft, &snap, true);
            assert!(!w.microsoft.button.is_sensitive(), "no registration, inert");
            assert_eq!(w.microsoft.note.label(), S::MICROSOFT_UNAVAILABLE);
            assert!(!w.microsoft.code_row.is_visible());

            snap.microsoft_signin_available = true;
            render_microsoft_rows(&w.microsoft, &snap, true);
            assert!(w.microsoft.button.is_sensitive());
            assert!(!w.microsoft.note.is_visible());

            snap.microsoft_signin = Some(fauna_client_mail_settings::MicrosoftSignInView {
                user_code: "FAUNA-42".into(),
                verification_uri: "https://microsoft.example/devicelogin".into(),
                expires_at_ms: 0,
                state: MicrosoftSignInState::Waiting,
            });
            render_microsoft_rows(&w.microsoft, &snap, true);
            assert!(w.microsoft.code_row.is_visible());
            assert!(!w.microsoft.button.is_sensitive(), "in flight, inert");
            assert_eq!(w.microsoft.note.label(), S::MICROSOFT_WAITING);
            assert_eq!(
                w.microsoft.code_label.text(),
                S::microsoft_code_fmt("https://microsoft.example/devicelogin", "FAUNA-42")
            );
            assert_eq!(
                crate::testid::test_attr_text(&w.microsoft.code_label, "code").as_deref(),
                Some("FAUNA-42")
            );
            assert_eq!(
                crate::testid::test_attr_text(&w.microsoft.code_label, "url").as_deref(),
                Some("https://microsoft.example/devicelogin")
            );

            render_microsoft_rows(&w.microsoft, &snap, false);
            assert!(!w.microsoft.row.is_visible() && !w.microsoft.code_row.is_visible());
        });
    }

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
                byte_total: None,
            },
            SourceMailboxOption {
                name: "Trash".into(),
                selected: false,
                message_count: 99,
                byte_total: None,
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

    /// The timed forms: the Done card says how long the run took, the progress
    /// summary adds elapsed time — both from the shared `transfer_timing`.
    #[test]
    fn timed_summaries_render_through_the_shared_timing() {
        let mut snap = a_snapshot();
        snap.started_at = 1_000;
        snap.last_progress_at = 1_090; // session_state None → not running → 90 s
        snap.imported_count = 12;
        snap.skipped_count = 4;
        snap.errored_count = 1;
        snap.total_count = 20;
        let took = duration_text(90);
        assert_eq!(
            done_text(&snap),
            S::done_summary_timed_fmt("12", "4", "1", &took)
        );
        assert!(done_text(&snap).contains("took"), "{}", done_text(&snap));
        assert_eq!(
            progress_text(&snap),
            S::progress_elapsed_fmt(&S::progress_summary_fmt("12", "20", "4", "1"), &took)
        );
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
                assert!(
                    !w.microsoft.row.is_visible(),
                    "{kind:?} has no OAuth button"
                );
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

            // Outlook: the address and Sign in with Microsoft — no password, no
            // host, no Connect (the IMAP fallback is withdrawn).
            apply_source_visibility(&w, ImportSourceKind::Outlook);
            assert!(w.username_row.is_visible(), "Outlook asks for the address");
            assert_eq!(w.username_row.title(), S::SOURCE_OUTLOOK_ADDRESS_LABEL);
            assert!(w.microsoft.row.is_visible(), "Outlook shows the sign-in");
            assert!(
                !w.app_password_row.is_visible(),
                "Outlook has no app-password"
            );
            for (name, row) in [
                ("host", &w.host_row),
                ("port", &w.port_row),
                ("tls-mode", &w.tls_mode_row),
                ("password", &w.password_row),
                ("connect", &w.connect_row),
            ] {
                assert!(!row.is_visible(), "Outlook must not show {name}");
            }

            // Generic: the IMAP fields alone.
            apply_source_visibility(&w, ImportSourceKind::Generic);
            assert!(!w.microsoft.row.is_visible(), "Generic has no OAuth button");
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
                ("connect", &w.connect_row),
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

    /// A failed import offers Dismiss and none of the controls; a live one the
    /// reverse, with the restart only while a rebuilt mailbox is pending.
    /// Mirrors tui's `a_failed_import_offers_dismiss_and_none_of_the_three_controls`.
    #[test]
    fn a_failed_import_offers_dismiss_and_none_of_the_controls() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let controls = adw::ActionRow::new();
            let dismiss = adw::ActionRow::new();
            let restart = gtk::Button::new();
            let mut snap = a_mail_import_snapshot(ImportStep::Progress);

            snap.session_state = Some(ImportSessionState::Errored);
            apply_progress_controls(&controls, &restart, &dismiss, &snap);
            assert!(!controls.is_visible() && dismiss.is_visible());

            snap.session_state = Some(ImportSessionState::Running);
            apply_progress_controls(&controls, &restart, &dismiss, &snap);
            assert!(controls.is_visible() && !dismiss.is_visible());
            assert!(
                !restart.is_visible(),
                "no rebuilt mailbox, no restart offer"
            );

            snap.rebuilt_mailboxes = vec!["INBOX".into()];
            apply_progress_controls(&controls, &restart, &dismiss, &snap);
            assert!(restart.is_visible());
        });
    }

    /// The remembered source fills the Source drafts once per visit, never over
    /// what the user typed, and a landed Forget empties them.
    #[test]
    fn the_remembered_source_fills_the_drafts_once_and_forget_empties_them() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let (_page, w, _buttons) = build_page_parts();
            let seeded = Cell::new(false);
            let was = Cell::new(false);
            let mut snap = a_mail_import_snapshot(ImportStep::Source);
            snap.source_remembered = true;
            snap.host = "imap.old.example".into();
            snap.port = 993;
            snap.username = "me@old.example".into();

            sync_remembered_drafts(&w, &seeded, &was, &snap);
            assert_eq!(w.host_input.text(), "imap.old.example");
            assert_eq!(w.port_input.text(), "993");
            assert_eq!(w.username_input.text(), "me@old.example");

            w.username_input.set_text("typed@example.com");
            sync_remembered_drafts(&w, &seeded, &was, &snap);
            assert_eq!(w.username_input.text(), "typed@example.com");

            snap.source_remembered = false;
            sync_remembered_drafts(&w, &seeded, &was, &snap);
            assert!(w.host_input.text().is_empty() && w.username_input.text().is_empty());
        });
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
                "mail-import-source-oauth-code",
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
                "mail-import-restart-button",
                "mail-import-dismiss-button",
                "mail-import-source-forget-button",
                "mail-import-scope-import-duplicates-toggle",
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
