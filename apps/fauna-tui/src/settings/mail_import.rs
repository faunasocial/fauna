//! The Settings → Mail → Import mailbox wizard (`docs/goal/behavior/
//! mailbox-migration.md` § UX shape → § Wizard steps; `tests/e2e-unified/
//! ui.yaml` `mail-import` page + its `mail-import-mailbox-progress-list`
//! component).
//!
//! Where a person pulls their existing mail from a foreign IMAP server
//! (Gmail / Outlook / iCloud / generic) into their Fauna mailbox — a
//! five-screen wizard (Source → Scope → Confirm → Progress → Done) that is a
//! dumb renderer of the shared `fauna_client_mail_settings::MailImportSnapshot`
//! and a dispatcher of `MailImportAction`. The FSM (step transitions, mailbox
//! default-selection, the fetch-drive loop) is shared Rust (priority #2/#4);
//! tui is the lead app — no other app renders this page yet
//! (`mailbox-migration.md` § Implementation status today: "no app wires the
//! machine into a rendered page yet — that build (tui first, per the lead-app
//! ordering) is the remaining Track D work").
//!
//! # Unlike `mail_export.rs`, the backend is REAL
//!
//! Every Import RPC has shipped since 2026-07-08 (§ Implementation status
//! today), and both machine seams (`MailImportNest`, `ImportSourceNest`) are
//! real, not stubs. So `Start`/`Pause`/`Resume`/`Cancel` drive a genuine
//! `import_sessions` row, and `Connect` opens a genuine session against the
//! foreign IMAP source. A rejection (bad credentials, a locked source, an
//! over-quota mailbox, …) is a real nest/source answer, bridged onto
//! `error-message` by `apply_mail_import_snapshot` — never a fake-green, never
//! a dropped command (testing.md point 11).
//!
//! # Per-provider Source-step field visibility
//!
//! Not spelled out as a rule anywhere, but `mailbox-migration.md` § Wizard
//! steps step 1's "Required fields" language settles it: only **Generic IMAP**
//! lists host/port/TLS-mode/username/password as required. So:
//!
//! - **Gmail / iCloud** paint `source-app-password` + `source-username` only —
//!   host/port/tls-mode stay on the provider preset
//!   (`MailImportMachine::apply_client_action`'s `SelectSourceKind` arm),
//!   unpainted.
//! - **Outlook** paints `source-username` (the Microsoft account's address,
//!   the XOAUTH2 user) and **Sign in with Microsoft** — no password field, no
//!   host field: Microsoft takes no password from a mail app, so the fallback
//!   is withdrawn and the triple is a preset (`mailbox-migration.md`
//!   § Microsoft sign-in — the device-code grant). No Connect button either:
//!   the grant connects by itself.
//! - **Generic** paints host/port/tls-mode/username/password; no app-password,
//!   no OAuth button.
//!
//! `source-username` is the one field painted for every kind — nothing in
//! ui.yaml scopes it to a subset the way the other four are annotated.
//!
//! # Every Source/Scope text field is a page-local draft buffer
//!
//! `MailImportAction::Set*` are all cheap, client-only mutations inside the
//! machine's own `dispatch` (no RPC — `import.rs::apply_client_action`), but
//! each dispatch still crosses tui's Op plumbing, so committing per keystroke
//! would still be one op per character — `mail_export.rs`'s date-buffer
//! reasoning, generalized here to every text field on this page. Buffers
//! commit as one multi-action dispatch at the transition point: Connect for
//! Source-step fields (`connect_actions`), Next for Scope-step fields
//! (`scope_next_actions`).
//!
//! # `mail-import-scope-mailbox-mapping` is informational only
//!
//! No `MailImportAction` exists to change the destination mailbox mapping —
//! the doc names no control for it either (§ Wizard steps step 3: "1:1 by
//! default … source mailboxes with names matching no Fauna standard mailbox
//! land as user-created mailboxes named after the source"). Painted as a plain
//! label describing the rule.
//!
//! # The two Done-step buttons have no machine action
//!
//! `mail-import-view-imported-button` / `mail-import-review-skipped-button`
//! are not backed by any `MailImportAction`. View imported uses the generic
//! `Gesture::Nav(Page::Conversations)` (mail lives on the Conversations
//! page). Review skipped toggles the snapshot's skip log onto the Done screen
//! (`MailImportState::show_skipped`, `mailbox-migration.md` § Wizard steps
//! step 6) — the log is already on the snapshot, kept past the run's end, so
//! revealing it is a view toggle, the archive import's own shape.
//!
//! # A session adopted after a restart asks for its credential on Progress
//!
//! When the machine adopted a live session cold
//! (`MailImportSnapshot::resume_needs_credentials`, `mailbox-migration.md`
//! § Resume protocol step 2), the Progress step paints the Source step's own
//! credential field for the session's provider beside Resume
//! (`resume_credential_elements`), and Resume commits it first through the
//! shared `resume_actions`.
//!
//! # The Outlook source signs in with Microsoft
//!
//! `mail-import-source-oauth-button` dispatches the shared
//! `microsoft_sign_in_actions` (commit the address, `BeginMicrosoftSignIn`);
//! `Op::MailImportDispatch`'s executor then spawns
//! `MailImportMachine::run_microsoft_sign_in`, the polling half, exactly as it
//! spawns `run_import`. While the code is out, `mail-import-source-oauth-code`
//! paints the code and Microsoft's address (its `code` and `url` attributes
//! carry each alone) with an id-less **Open in browser** beside it, through
//! `os_open` — the person may equally type the code on a phone. The
//! page's one-second tick (`settings::mail_import_progress_active`) repaints
//! while the sign-in is in flight, and a grant moves the step on by itself;
//! a declined, expired or consent-refused sign-in lands its sentence on
//! `error-message`. Until the platform can sign in at all
//! (`MailImportSnapshot::microsoft_signin_available` — the project's
//! application ID still empty) the button paints inert beside
//! `mail_import.microsoft_unavailable`, never a failing request.

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_client_mail_settings::{
    DEFAULT_MAX_SIZE_MB, ImportPauseReason, ImportSessionState, ImportSourceKind, ImportStep,
    ImportTlsMode, MailImportAction, MailImportMachine, MailImportSnapshot, MicrosoftSignInState,
    SOURCE_KINDS, TLS_MODES, import_source_kind_label, import_tls_mode_label,
};
use fauna_core::secret::SecretString;
use fauna_i18n::strings::mail_import as t;

use super::{Action, SettingsField, SettingsState};
use crate::element::{Element, Field, Gesture, SelectTarget};
use crate::pages::Page;

/// The Import sub-page's state.
///
/// The machine is built once at the post-auth hook (`attach_session`), the
/// `MailExportState` shape — construction is sync, cheap and infallible.
pub(crate) struct MailImportState {
    /// The shared wizard machine. `None` pre-auth.
    pub(super) machine: Option<Arc<MailImportMachine>>,
    /// The last rendered snapshot. `None` until the first hydrate resolves.
    pub(super) snapshot: Option<MailImportSnapshot>,
    /// Step 1 (Generic) — local drafts, committed by
    /// `connect_actions` on Connect.
    pub(super) host_input: String,
    pub(super) port_input: String,
    /// Step 1 (every kind) — local draft, committed on Connect.
    pub(super) username_input: String,
    /// Step 1 (every kind) — ONE buffer behind both
    /// `mail-import-source-password` and `mail-import-source-app-password`
    /// (`SettingsField::MailImportPassword`'s doc comment).
    pub(super) password_input: String,
    /// Step 3 — local draft, committed by `scope_next_actions` on Next.
    pub(super) date_from_input: String,
    /// Step 3 — **MB**, not bytes (the machine's own unit); local draft,
    /// converted + committed by `scope_next_actions` on Next.
    pub(super) max_size_input: String,
    /// Set once Cancel has been armed by a first click — the
    /// `MailExportState::cancel_armed` shape.
    pub(super) cancel_armed: bool,
    /// Whether Review skipped has revealed the skip log on the Done screen
    /// — the `ArchiveImportState::show_skipped` shape.
    pub(super) show_skipped: bool,
    /// Whether this visit's Source drafts were filled from the remembered
    /// source already (`settings::apply_mail_import_snapshot`) — once per
    /// visit, so a repaint never refills a field the user emptied.
    pub(super) drafts_seeded: bool,
}

impl Default for MailImportState {
    fn default() -> Self {
        Self {
            machine: None,
            snapshot: None,
            host_input: String::new(),
            port_input: String::new(),
            username_input: String::new(),
            password_input: String::new(),
            date_from_input: String::new(),
            max_size_input: DEFAULT_MAX_SIZE_MB.to_string(),
            cancel_armed: false,
            show_skipped: false,
            drafts_seeded: false,
        }
    }
}

impl MailImportState {
    /// Build the page's shared machine from the session's WS handle. No
    /// keypair: the import RPCs are user-tier, so the nest derives the owning
    /// actor from the authenticated caller (the Export reasoning).
    pub(super) fn build(
        nest: Arc<fauna_client::NestClient>,
        memory: Arc<dyn fauna_client_config::MailImportSourceStore>,
    ) -> Self {
        Self {
            machine: Some(Arc::new(
                crate::mail_glue::build_mail_import_machine(nest).with_source_memory(memory),
            )),
            ..Self::default()
        }
    }

    /// Drop the page-local drafts on a fresh visit: the armed cancel (the
    /// `MailExportState::reset_form` shape) and every Source/Scope buffer,
    /// which would otherwise paint stale credentials or a scope the machine no
    /// longer holds.
    pub(super) fn reset_form(&mut self) {
        self.cancel_armed = false;
        self.show_skipped = false;
        self.drafts_seeded = false;
        self.host_input.clear();
        self.port_input.clear();
        self.username_input.clear();
        self.password_input.clear();
        self.date_from_input.clear();
        self.max_size_input = DEFAULT_MAX_SIZE_MB.to_string();
    }

    /// Step 1→2: commit whichever Source-step fields `kind` actually shows
    /// (the module docs' per-provider visibility table), then Connect — one
    /// awaited op. The wire-action shape itself is
    /// `fauna_client_mail_settings::connect_actions` (shared with linux);
    /// this side's own job is just locating the strings in `self`.
    pub(super) fn connect_actions(&self, kind: ImportSourceKind) -> Vec<MailImportAction> {
        fauna_client_mail_settings::connect_actions(
            kind,
            &self.host_input,
            &self.port_input,
            &self.username_input,
            SecretString::from(self.password_input.clone()),
        )
    }

    /// Step 3→4: commit both Scope-step drafts, then advance. The wire-action
    /// shape itself is `fauna_client_mail_settings::scope_next_actions`
    /// (shared with linux); this side's own job is just locating the strings
    /// in `self` — same division of labor as [`Self::connect_actions`].
    pub(super) fn scope_next_actions(&self) -> Vec<MailImportAction> {
        fauna_client_mail_settings::scope_next_actions(&self.date_from_input, &self.max_size_input)
    }

    /// Step 5's Resume: on a session adopted after a restart, commit the
    /// credential the Progress step painted first. The wire-action shape is
    /// `fauna_client_mail_settings::resume_actions`; this side's own job is
    /// locating the strings in `self`, as for [`Self::connect_actions`].
    pub(super) fn resume_actions(&self) -> Vec<MailImportAction> {
        let (needs, kind) = self
            .snapshot
            .as_ref()
            .map_or((false, ImportSourceKind::Gmail), |s| {
                (s.resume_needs_credentials, s.source_kind)
            });
        fauna_client_mail_settings::resume_actions(
            needs,
            kind,
            &self.port_input,
            SecretString::from(self.password_input.clone()),
        )
    }
}

fn source_kind_label(kind: ImportSourceKind) -> String {
    crate::wizard::localized(&import_source_kind_label(kind))
}

fn source_kind_options() -> Vec<String> {
    SOURCE_KINDS.into_iter().map(source_kind_label).collect()
}

/// Map a picker label back to its [`ImportSourceKind`]. An unrecognized label
/// keeps the ratified default (Gmail) rather than panicking (the
/// `mail_export::format_for_label` contract).
pub(crate) fn source_kind_for_label(label: &str) -> ImportSourceKind {
    SOURCE_KINDS
        .into_iter()
        .find(|k| source_kind_label(*k) == label)
        .unwrap_or(ImportSourceKind::Gmail)
}

fn tls_mode_label(mode: ImportTlsMode) -> String {
    crate::wizard::localized(&import_tls_mode_label(mode))
}

fn tls_mode_options() -> Vec<String> {
    TLS_MODES.into_iter().map(tls_mode_label).collect()
}

/// Map a picker label back to its [`ImportTlsMode`]. An unrecognized label
/// keeps the ratified default (Implicit) rather than panicking.
pub(crate) fn tls_mode_for_label(label: &str) -> ImportTlsMode {
    TLS_MODES
        .into_iter()
        .find(|m| tls_mode_label(*m) == label)
        .unwrap_or(ImportTlsMode::Implicit)
}

/// The Import sub-page's ordered element list — only the ACTIVE step's ids,
/// the `mail_export_elements` shape.
///
/// The page's `error-message` is registered globally by
/// [`crate::ui::register_frame`], so a snapshot error bridges onto it through
/// `App::errors` rather than being painted here.
pub(super) fn mail_import_elements(state: &SettingsState) -> Vec<Element> {
    let e = &state.mail_import;
    let snapshot = e.snapshot.as_ref();
    // Pre-hydrate the wizard still opens on step 1 with the default provider:
    // the client-side steps do not depend on the nest.
    let step = snapshot.map(|s| s.step).unwrap_or(ImportStep::Source);
    let kind = snapshot
        .map(|s| s.source_kind)
        .unwrap_or(ImportSourceKind::Gmail);
    let tls_mode = snapshot
        .map(|s| s.tls_mode)
        .unwrap_or(ImportTlsMode::Implicit);

    let mut els = vec![
        Element::label(ids::PAGE_HEADING, t::TITLE),
        Element::chrome(t::DESCRIPTION),
    ];

    match step {
        ImportStep::Source => els.extend(source_step_elements(e, kind, tls_mode)),
        ImportStep::Scope => els.extend(scope_step_elements(e, snapshot)),
        ImportStep::Confirm => els.extend(confirm_step_elements(snapshot)),
        ImportStep::Progress => els.extend(progress_step_elements(e, snapshot)),
        ImportStep::Done => els.extend(done_step_elements(e, snapshot)),
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

/// Step 1 — source provider + credentials. See the module docs' per-provider
/// visibility table.
fn source_step_elements(
    e: &MailImportState,
    kind: ImportSourceKind,
    tls_mode: ImportTlsMode,
) -> Vec<Element> {
    let mut els = vec![
        Element::chrome(t::SOURCE_TITLE),
        Element::select(
            ids::MAIL_IMPORT_SOURCE_PICKER,
            source_kind_label(kind),
            SelectTarget::MailImportSourceKind,
            source_kind_options(),
        )
        .labelled(t::SOURCE_TITLE),
    ];

    match kind {
        ImportSourceKind::Gmail | ImportSourceKind::ICloud => {
            let help = if kind == ImportSourceKind::Gmail {
                t::SOURCE_APP_PASSWORD_HELP_GMAIL
            } else {
                t::SOURCE_APP_PASSWORD_HELP_ICLOUD
            };
            els.push(
                Element::input(
                    ids::MAIL_IMPORT_SOURCE_USERNAME,
                    e.username_input.clone(),
                    Field::Settings(SettingsField::MailImportUsername),
                )
                .labelled(t::SOURCE_USERNAME_PLACEHOLDER),
            );
            // The explanation rides the field as its `help` attribute too, so
            // the promise "an app password is needed, made here" is
            // observable on the field it is about (§ Wizard steps step 1).
            els.push(
                Element::input(
                    ids::MAIL_IMPORT_SOURCE_APP_PASSWORD,
                    crate::unlock::mask(e.password_input.as_str()),
                    Field::Settings(SettingsField::MailImportPassword),
                )
                .labelled(t::SOURCE_APP_PASSWORD_LABEL)
                .attr("help", help),
            );
            els.push(Element::chrome(help));
        }
        ImportSourceKind::Outlook => {
            els.push(
                Element::input(
                    ids::MAIL_IMPORT_SOURCE_USERNAME,
                    e.username_input.clone(),
                    Field::Settings(SettingsField::MailImportUsername),
                )
                .labelled(t::SOURCE_OUTLOOK_ADDRESS_LABEL),
            );
            els.extend(microsoft_sign_in_elements(e.snapshot.as_ref()));
            els.extend(forget_source_element(e.snapshot.as_ref()));
            // The grant connects by itself — no Connect button to press.
            return els;
        }
        ImportSourceKind::Generic => {
            els.extend(generic_imap_fields(e, tls_mode));
        }
    }

    els.extend(forget_source_element(e.snapshot.as_ref()));
    els.push(Element::gesture_button(
        ids::MAIL_IMPORT_CONNECT_BUTTON,
        t::CONNECT_BUTTON,
        true,
        Gesture::Settings(Action::MailImportConnect),
    ));
    els
}

/// `mail-import-source-forget-button` — painted for every provider while the
/// account remembers a source (`MailImportSnapshot::source_remembered`,
/// `mailbox-migration.md` § Credential handling).
fn forget_source_element(snapshot: Option<&MailImportSnapshot>) -> Option<Element> {
    snapshot.filter(|s| s.source_remembered).map(|_| {
        Element::gesture_button(
            ids::MAIL_IMPORT_SOURCE_FORGET_BUTTON,
            t::SOURCE_FORGET_BUTTON,
            true,
            Gesture::Settings(Action::MailImportForgetSource),
        )
    })
}

/// Whether the Outlook source's sign-in is under way — a code requested, out,
/// or its grant connecting. The button stays inert meanwhile, and the page's
/// tick keeps repainting (`settings::mail_import_progress_active`).
pub(super) fn microsoft_sign_in_in_flight(snapshot: &MailImportSnapshot) -> bool {
    snapshot.microsoft_signin.as_ref().is_some_and(|v| {
        matches!(
            v.state,
            MicrosoftSignInState::Requesting
                | MicrosoftSignInState::Waiting
                | MicrosoftSignInState::Connecting
        )
    })
}

/// The Outlook source's sign-in (module docs): the button — inert beside the
/// unavailable sentence until this platform can sign in, and while a sign-in
/// is in flight — then, once Microsoft has answered, the code and its address
/// with Open in browser. Shared by the Source step and an adopted session's
/// Progress step (`resume_credential_elements`).
fn microsoft_sign_in_elements(snapshot: Option<&MailImportSnapshot>) -> Vec<Element> {
    let available = snapshot.is_some_and(|s| s.microsoft_signin_available);
    let in_flight = snapshot.is_some_and(microsoft_sign_in_in_flight);
    let mut els = vec![Element::gesture_button(
        ids::MAIL_IMPORT_SOURCE_OAUTH_BUTTON,
        t::SOURCE_OAUTH_BUTTON,
        available && !in_flight,
        Gesture::Settings(Action::MailImportOAuthStart),
    )];
    if !available {
        els.push(Element::chrome(t::MICROSOFT_UNAVAILABLE));
        return els;
    }
    let Some(view) = snapshot.and_then(|s| s.microsoft_signin.as_ref()) else {
        return els;
    };
    if view.state == MicrosoftSignInState::Waiting && !view.user_code.is_empty() {
        els.push(
            Element::label(
                ids::MAIL_IMPORT_SOURCE_OAUTH_CODE,
                t::microsoft_code_fmt(&view.verification_uri, &view.user_code),
            )
            .attr("code", view.user_code.clone())
            .attr("url", view.verification_uri.clone()),
        );
        els.push(Element::chrome_button(
            t::MICROSOFT_OPEN_BROWSER,
            Gesture::Settings(Action::MailImportOpenMicrosoftSignIn),
        ));
        els.push(Element::chrome(t::MICROSOFT_WAITING));
    }
    els
}

/// The host/port/tls-mode/username/password field group — the Generic
/// source's required set (`mailbox-migration.md` § Wizard steps step 1).
fn generic_imap_fields(e: &MailImportState, tls_mode: ImportTlsMode) -> Vec<Element> {
    vec![
        Element::input(
            ids::MAIL_IMPORT_SOURCE_HOST,
            e.host_input.clone(),
            Field::Settings(SettingsField::MailImportHost),
        )
        .labelled(t::SOURCE_HOST_PLACEHOLDER),
        Element::input(
            ids::MAIL_IMPORT_SOURCE_PORT,
            e.port_input.clone(),
            Field::Settings(SettingsField::MailImportPort),
        )
        .labelled(t::SOURCE_PORT_PLACEHOLDER),
        Element::select(
            ids::MAIL_IMPORT_SOURCE_TLS_MODE,
            tls_mode_label(tls_mode),
            SelectTarget::MailImportTlsMode,
            tls_mode_options(),
        ),
        Element::input(
            ids::MAIL_IMPORT_SOURCE_USERNAME,
            e.username_input.clone(),
            Field::Settings(SettingsField::MailImportUsername),
        )
        .labelled(t::SOURCE_USERNAME_PLACEHOLDER),
        Element::input(
            ids::MAIL_IMPORT_SOURCE_PASSWORD,
            crate::unlock::mask(e.password_input.as_str()),
            Field::Settings(SettingsField::MailImportPassword),
        )
        .labelled(t::SOURCE_PASSWORD_PLACEHOLDER),
    ]
}

/// Step 3 — scope. The mailbox multi-select, the date buffer, the max-size
/// buffer, the mapping info line, and the advanced "Import duplicates anyway"
/// toggle, whose label carries its double-storage warning
/// (`mailbox-migration.md` § Opt-out per session, § Don't do these).
///
/// The per-mailbox checkboxes carry the REAL, indexed `mail-import-scope-
/// mailbox-item` id (user-approved 2026-08-28), unlike `mail_export.rs`'s
/// equivalent rows: unlike Export, this wizard's backend is real and this
/// row's own success bar needs a genuine e2e walk, which needs the rows —
/// and the Scope↔Confirm `Next`/`Back` navigation below — to be clickable at
/// all. `mail-export.md`'s per-row and Next/Back gap is untouched by this
/// change (flagged, not fixed, in the same approval — TODO owed there).
fn scope_step_elements(e: &MailImportState, snapshot: Option<&MailImportSnapshot>) -> Vec<Element> {
    let mailboxes = snapshot.map(|s| s.mailboxes.as_slice()).unwrap_or(&[]);
    let import_duplicates = snapshot.is_some_and(|s| s.import_duplicates);

    let mut els = vec![
        Element::chrome(t::SCOPE_TITLE),
        Element::label(ids::MAIL_IMPORT_SCOPE_MAILBOXES, t::SCOPE_MAILBOXES_LABEL),
    ];
    if mailboxes.is_empty() {
        els.push(Element::chrome(t::SCOPE_MAILBOXES_EMPTY));
    }
    for mailbox in mailboxes {
        els.push(
            Element::checkbox_gesture(
                ids::MAIL_IMPORT_SCOPE_MAILBOX_ITEM,
                mailbox.name.clone(),
                mailbox.selected,
                Gesture::Settings(Action::MailImportToggleMailbox(mailbox.name.clone())),
            )
            .attr("state", if mailbox.selected { "on" } else { "off" }),
        );
    }

    els.push(
        Element::input(
            ids::MAIL_IMPORT_SCOPE_DATE_FROM,
            e.date_from_input.clone(),
            Field::Settings(SettingsField::MailImportDateFrom),
        )
        .labelled(t::SCOPE_DATE_FROM_PLACEHOLDER),
    );
    els.push(
        Element::input(
            ids::MAIL_IMPORT_SCOPE_MAX_SIZE,
            e.max_size_input.clone(),
            Field::Settings(SettingsField::MailImportMaxSize),
        )
        .labelled(t::SCOPE_MAX_SIZE_LABEL),
    );
    els.push(Element::label(
        ids::MAIL_IMPORT_SCOPE_MAILBOX_MAPPING,
        t::SCOPE_MAILBOX_MAPPING_LABEL,
    ));
    els.push(
        Element::checkbox_gesture(
            ids::MAIL_IMPORT_SCOPE_IMPORT_DUPLICATES_TOGGLE,
            t::SCOPE_IMPORT_DUPLICATES_LABEL,
            import_duplicates,
            Gesture::Settings(Action::MailImportToggleImportDuplicates),
        )
        .attr("state", if import_duplicates { "on" } else { "off" }),
    );
    els.push(Element::gesture_button(
        ids::WIZARD_BACK_BUTTON,
        t::BACK,
        true,
        Gesture::Settings(Action::MailImportBack),
    ));
    els.push(Element::gesture_button(
        ids::WIZARD_NEXT_BUTTON,
        t::NEXT,
        true,
        Gesture::Settings(Action::MailImportNext),
    ));
    els
}

/// Step 4 — confirmation + the durable commit.
///
/// The summary states the source and the selected mailboxes' estimate —
/// messages and, when every one of them was sized, roughly how much data
/// (`mailbox-migration.md` § Wizard steps step 4), from
/// `MailImportSnapshot::selected_totals`.
fn confirm_step_elements(snapshot: Option<&MailImportSnapshot>) -> Vec<Element> {
    let source = source_kind_label(snapshot.map_or(ImportSourceKind::Gmail, |s| s.source_kind));
    let totals = snapshot.map(MailImportSnapshot::selected_totals);
    let mailboxes = totals.map_or(0, |t| t.mailboxes).to_string();
    let messages = totals.map_or(0, |t| t.messages).to_string();
    let summary = match totals.and_then(|t| t.bytes) {
        Some(bytes) => t::confirm_summary_sized_fmt(
            &source,
            &mailboxes,
            &messages,
            &crate::wizard::localized(&fauna_core::format::byte_size(bytes)),
        ),
        None => t::confirm_summary_fmt(&source, &mailboxes, &messages),
    };
    vec![
        Element::chrome(t::CONFIRM_TITLE),
        Element::label(ids::MAIL_IMPORT_CONFIRM_SUMMARY, summary),
        Element::gesture_button(
            ids::WIZARD_BACK_BUTTON,
            t::BACK,
            true,
            Gesture::Settings(Action::MailImportBack),
        ),
        Element::gesture_button(
            ids::MAIL_IMPORT_START_BUTTON,
            t::START_BUTTON,
            true,
            Gesture::Settings(Action::MailImportStart),
        ),
    ]
}

/// Step 5 — progress. Overall counters, the bar, pause/resume/cancel, the
/// per-message error/skip log, and the per-mailbox progress rows.
fn progress_step_elements(
    e: &MailImportState,
    snapshot: Option<&MailImportSnapshot>,
) -> Vec<Element> {
    let (imported, total, skipped, errored) = snapshot
        .map(|s| {
            (
                s.imported_count,
                s.total_count,
                s.skipped_count,
                s.errored_count,
            )
        })
        .unwrap_or((0, 0, 0, 0));
    // The SHARED zero-guarded, clamped ratio — the `mail_export.rs` precedent.
    let fraction = fauna_core::format::quota_fraction(imported as i64, total as i64);

    let mut els = vec![
        Element::chrome(t::PROGRESS_TITLE),
        Element::label(
            ids::MAIL_IMPORT_PROGRESS_SUMMARY,
            with_timing(
                t::progress_summary_fmt(
                    &imported.to_string(),
                    &total.to_string(),
                    &skipped.to_string(),
                    &errored.to_string(),
                ),
                snapshot.map(|s| s.timing(fauna_core::data::Timestamp::now_secs())),
            ),
        ),
        Element::label(
            ids::MAIL_IMPORT_PROGRESS_BAR,
            format!("{:.0}%", fraction * 100.0),
        )
        .attr("fraction", format!("{fraction:.4}")),
    ];
    let failed = snapshot.and_then(|s| s.session_state) == Some(ImportSessionState::Errored);
    if failed {
        // A failed import is a record, not a run (`mailbox-migration.md` § A
        // failed import is a record the wizard shows until the user moves
        // on): none of the three controls applies, and its one control is
        // Dismiss. Its reason is the error log's last line.
        els.push(Element::gesture_button(
            ids::MAIL_IMPORT_DISMISS_BUTTON,
            t::DISMISS_FAILED_BUTTON,
            true,
            Gesture::Settings(Action::MailImportDismiss),
        ));
    } else {
        if let Some(s) = snapshot.filter(|s| s.resume_needs_credentials) {
            els.extend(resume_credential_elements(e, s));
        }
        els.extend([
            Element::gesture_button(
                ids::MAIL_IMPORT_PAUSE_BUTTON,
                t::PAUSE_BUTTON,
                true,
                Gesture::Settings(Action::MailImportPause),
            ),
            Element::gesture_button(
                ids::MAIL_IMPORT_RESUME_BUTTON,
                t::RESUME_BUTTON,
                true,
                Gesture::Settings(Action::MailImportResume),
            ),
        ]);
        // A source mailbox the old provider rebuilt (§ Resume protocol step
        // 4): its warning is already in the error log; the restart is offered
        // while one is pending.
        if snapshot.is_some_and(|s| !s.rebuilt_mailboxes.is_empty()) {
            els.push(Element::gesture_button(
                ids::MAIL_IMPORT_RESTART_BUTTON,
                t::RESTART_BUTTON,
                true,
                Gesture::Settings(Action::MailImportRestart),
            ));
        }
        // Two-click inline confirm: the relabel is the gate's only visible
        // affordance, the `mail_export.rs` shape.
        els.push(Element::gesture_button(
            ids::MAIL_IMPORT_CANCEL_BUTTON,
            if e.cancel_armed {
                fauna_i18n::strings::common::CONFIRM_Q
            } else {
                t::CANCEL_BUTTON
            },
            true,
            Gesture::Settings(Action::MailImportCancel),
        ));
    }
    // The full-mailbox notice (`mailbox-migration.md` § Failure handling →
    // *Nest-side errors*): ask the admin then Resume, Skip the rest, or
    // Cancel — the first and last are the buttons above.
    if snapshot.is_some_and(|s| {
        s.pause_reason == Some(ImportPauseReason::MailboxFull)
            && s.session_state == Some(ImportSessionState::Paused)
    }) {
        els.extend([
            Element::label(ids::MAIL_IMPORT_QUOTA_NOTICE, t::QUOTA_NOTICE),
            Element::gesture_button(
                ids::MAIL_IMPORT_QUOTA_SKIP_REST_BUTTON,
                t::QUOTA_SKIP_REST_BUTTON,
                true,
                Gesture::Settings(Action::MailImportSkipRest),
            ),
        ]);
    }
    els.extend([
        Element::label(
            ids::MAIL_IMPORT_ERROR_LOG,
            snapshot.map(|s| s.error_log.join("\n")).unwrap_or_default(),
        ),
        Element::chrome(t::ERROR_LOG_TITLE),
        Element::label(ids::MAIL_IMPORT_MAILBOX_PROGRESS_LIST, t::PROGRESS_TITLE),
    ]);
    // The machine keeps one row per mailbox the run takes, laid at Start.
    for row in snapshot
        .map(|s| s.mailbox_progress.as_slice())
        .unwrap_or(&[])
    {
        els.extend(mailbox_progress_row_elements(row));
    }
    els
}

/// The Progress step of a session this app adopted after a restart
/// (`MailImportSnapshot::resume_needs_credentials`, `mailbox-migration.md`
/// § Resume protocol step 2): the sentence saying why, and the Source step's
/// own credential field for the session's provider — the app password, or
/// Generic's password with its port and connection security, which the
/// session row does not record. The Outlook source asks for the Microsoft
/// sign-in again (module docs).
fn resume_credential_elements(e: &MailImportState, s: &MailImportSnapshot) -> Vec<Element> {
    match s.source_kind {
        ImportSourceKind::Gmail | ImportSourceKind::ICloud => vec![
            Element::chrome(t::RESUME_NEEDS_PASSWORD),
            Element::input(
                ids::MAIL_IMPORT_SOURCE_APP_PASSWORD,
                crate::unlock::mask(e.password_input.as_str()),
                Field::Settings(SettingsField::MailImportPassword),
            )
            .labelled(t::SOURCE_APP_PASSWORD_LABEL),
        ],
        ImportSourceKind::Generic => vec![
            Element::chrome(t::RESUME_NEEDS_PASSWORD),
            Element::input(
                ids::MAIL_IMPORT_SOURCE_PORT,
                e.port_input.clone(),
                Field::Settings(SettingsField::MailImportPort),
            )
            .labelled(t::SOURCE_PORT_PLACEHOLDER),
            Element::select(
                ids::MAIL_IMPORT_SOURCE_TLS_MODE,
                tls_mode_label(s.tls_mode),
                SelectTarget::MailImportTlsMode,
                tls_mode_options(),
            ),
            Element::input(
                ids::MAIL_IMPORT_SOURCE_PASSWORD,
                crate::unlock::mask(e.password_input.as_str()),
                Field::Settings(SettingsField::MailImportPassword),
            )
            .labelled(t::SOURCE_PASSWORD_PLACEHOLDER),
        ],
        ImportSourceKind::Outlook => {
            let mut els = vec![Element::chrome(t::RESUME_NEEDS_MICROSOFT)];
            els.extend(microsoft_sign_in_elements(Some(s)));
            els
        }
    }
}

/// One `mail-import-mailbox-progress-list-item` row. FLAT — the
/// `mail_export.rs::mailbox_progress_row_elements` shape: no `.within(..)`
/// scope, matching how `actions/mail_export.py` reads its own twin.
fn mailbox_progress_row_elements(
    row: &fauna_client_mail_settings::ImportMailboxProgress,
) -> Vec<Element> {
    vec![
        Element::label(
            ids::MAIL_IMPORT_MAILBOX_PROGRESS_LIST_ITEM,
            row.name.clone(),
        ),
        Element::label(
            ids::MAIL_IMPORT_MAILBOX_PROGRESS_LIST_ITEM_NAME,
            row.name.clone(),
        ),
        Element::label(
            ids::MAIL_IMPORT_MAILBOX_PROGRESS_LIST_ITEM_PROGRESS,
            fauna_client_mail_settings::import_mailbox_progress_text(row.clone())
                .resolve(fauna_i18n::strings::lookup),
        ),
    ]
}

/// The progress summary with the run's elapsed time and, while it is
/// running, the time left (`mailbox-migration.md` § Wizard steps step 5; the
/// shared `transfer_timing` decides both) — `mail_export.rs`'s twin.
fn with_timing(
    summary: String,
    timing: Option<fauna_client_mail_settings::TransferTiming>,
) -> String {
    match timing.map(|t| (t.elapsed_secs, t.remaining_secs)) {
        Some((Some(elapsed), Some(remaining))) => {
            t::progress_remaining_fmt(&summary, &duration_text(elapsed), &duration_text(remaining))
        }
        Some((Some(elapsed), None)) => t::progress_elapsed_fmt(&summary, &duration_text(elapsed)),
        _ => summary,
    }
}

/// A duration in the shared coarse format (`value-formatting.md` § Duration).
fn duration_text(secs: u64) -> String {
    fauna_core::format::duration_secs(secs).resolve(fauna_i18n::strings::lookup)
}

/// Step 6 — done. The summary card, View imported (Conversations), and Review
/// skipped, which reveals the run's skip log below the card (module docs).
fn done_step_elements(e: &MailImportState, snapshot: Option<&MailImportSnapshot>) -> Vec<Element> {
    let (imported, skipped, errored) = snapshot
        .map(|s| (s.imported_count, s.skipped_count, s.errored_count))
        .unwrap_or((0, 0, 0));
    // § Wizard steps step 6: the counts and the time elapsed.
    let elapsed = snapshot
        .and_then(|s| {
            s.timing(fauna_core::data::Timestamp::now_secs())
                .elapsed_secs
        })
        .map(duration_text);
    let summary = match elapsed {
        Some(elapsed) => t::done_summary_timed_fmt(
            &imported.to_string(),
            &skipped.to_string(),
            &errored.to_string(),
            &elapsed,
        ),
        None => t::done_summary_fmt(
            &imported.to_string(),
            &skipped.to_string(),
            &errored.to_string(),
        ),
    };
    let mut els = vec![
        Element::chrome(t::DONE_TITLE),
        Element::label(ids::MAIL_IMPORT_DONE_SUMMARY, summary),
        Element::gesture_button(
            ids::MAIL_IMPORT_VIEW_IMPORTED_BUTTON,
            t::VIEW_IMPORTED_BUTTON,
            true,
            Gesture::Nav(Page::Conversations),
        ),
        Element::gesture_button(
            ids::MAIL_IMPORT_REVIEW_SKIPPED_BUTTON,
            t::REVIEW_SKIPPED_BUTTON,
            true,
            Gesture::Settings(Action::MailImportToggleSkipped),
        ),
    ];
    if e.show_skipped {
        els.push(Element::chrome(t::ERROR_LOG_TITLE));
        els.push(Element::label(
            ids::MAIL_IMPORT_ERROR_LOG,
            snapshot.map(|s| s.error_log.join("\n")).unwrap_or_default(),
        ));
    }
    els
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::Role;
    use crate::settings::SubPage;
    use fauna_client_mail_settings::testing::a_mail_import_snapshot as a_snapshot;
    use fauna_client_mail_settings::{ImportMailboxProgress, SourceMailboxOption};

    fn state_with(snapshot: MailImportSnapshot) -> SettingsState {
        let mut state = SettingsState {
            sub: SubPage::MailImport,
            ..Default::default()
        };
        state.mail_import.snapshot = Some(snapshot);
        state
    }

    fn ids(els: &[Element]) -> Vec<String> {
        els.iter().map(|e| e.id.clone()).collect()
    }

    /// An Outlook Source step whose sign-in code is out: this platform can
    /// sign in, and Microsoft answered the button with a code.
    fn outlook_waiting() -> MailImportSnapshot {
        let mut snap = a_snapshot(ImportStep::Source);
        snap.source_kind = ImportSourceKind::Outlook;
        snap.microsoft_signin_available = true;
        snap.microsoft_signin = Some(fauna_client_mail_settings::MicrosoftSignInView {
            user_code: "FAUNA-42".into(),
            verification_uri: "https://microsoft.example/devicelogin".into(),
            expires_at_ms: 900_000,
            state: MicrosoftSignInState::Waiting,
        });
        snap
    }

    fn text_of(els: &[Element], id: &str, index: usize) -> String {
        els.iter()
            .filter(|e| e.id == id)
            .nth(index)
            .map(|e| e.text.clone())
            .unwrap_or_default()
    }

    /// Every static ui.yaml id for the `mail-import` page paints — collected
    /// across the five steps AND, on the Source step, across every provider
    /// kind (the per-kind visibility table hides some step-1 ids by default
    /// kind). The tui analogue of `mail_export.rs`'s
    /// `every_static_ui_yaml_id_paints_across_the_five_steps`.
    #[test]
    fn every_static_ui_yaml_id_paints_across_the_five_steps() {
        let mut painted: Vec<String> = Vec::new();
        for kind in SOURCE_KINDS {
            let mut snap = a_snapshot(ImportStep::Source);
            snap.source_kind = kind;
            painted.extend(ids(&mail_import_elements(&state_with(snap))));
        }
        // The Scope step needs at least one mailbox in the snapshot for
        // `mail-import-scope-mailbox-item` (conditional on a non-empty list,
        // the `SCOPE_MAILBOXES_EMPTY` placeholder's precedent) to paint at all.
        let mut scope_snap = a_snapshot(ImportStep::Scope);
        scope_snap.mailboxes = vec![fauna_client_mail_settings::SourceMailboxOption {
            name: "INBOX".into(),
            selected: true,
            message_count: 1,
            byte_total: None,
        }];
        painted.extend(ids(&mail_import_elements(&state_with(scope_snap))));
        for step in [ImportStep::Confirm, ImportStep::Progress, ImportStep::Done] {
            painted.extend(ids(&mail_import_elements(&state_with(a_snapshot(step)))));
        }
        let mut reviewing = state_with(a_snapshot(ImportStep::Done));
        reviewing.mail_import.show_skipped = true;
        painted.extend(ids(&mail_import_elements(&reviewing)));
        // The sign-in code paints only while Microsoft's code is out.
        painted.extend(ids(&mail_import_elements(&state_with(outlook_waiting()))));
        // The full-mailbox notice paints only on that pause.
        let mut full = a_snapshot(ImportStep::Progress);
        full.session_state = Some(ImportSessionState::Paused);
        full.pause_reason = Some(ImportPauseReason::MailboxFull);
        painted.extend(ids(&mail_import_elements(&state_with(full))));
        // Dismiss paints only on a failed import's Progress step.
        let mut failed = a_snapshot(ImportStep::Progress);
        failed.session_state = Some(ImportSessionState::Errored);
        painted.extend(ids(&mail_import_elements(&state_with(failed))));
        for id in [
            "page-heading",
            // "error-message" is registered globally by ui::register_frame
            // (the module docs), never painted by mail_import_elements
            // itself — the mail_export.rs precedent omits it too.
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
            "mail-import-scope-mailbox-item",
            "mail-import-scope-date-from",
            "mail-import-scope-max-size",
            "mail-import-scope-mailbox-mapping",
            "mail-import-scope-import-duplicates-toggle",
            "wizard-next-button",
            "wizard-back-button",
            "mail-import-confirm-summary",
            "mail-import-start-button",
            "mail-import-progress-summary",
            "mail-import-progress-bar",
            "mail-import-pause-button",
            "mail-import-resume-button",
            "mail-import-cancel-button",
            "mail-import-quota-notice",
            "mail-import-quota-skip-rest-button",
            "mail-import-dismiss-button",
            "mail-import-error-log",
            "mail-import-mailbox-progress-list",
            "mail-import-done-summary",
            "mail-import-view-imported-button",
            "mail-import-review-skipped-button",
            "settings-nav-back",
        ] {
            assert!(
                painted.contains(&id.to_string()),
                "missing {id:?}; painted {painted:?}"
            );
        }
    }

    /// The "Import duplicates anyway" toggle reflects the SNAPSHOT — off by
    /// default, flipped only by a dispatch that reached the machine — and its
    /// own label carries the double-storage warning (§ Opt-out per session,
    /// § Don't do these: never suppressed).
    #[test]
    fn the_import_duplicates_toggle_is_off_by_default_and_carries_its_warning() {
        let toggle = |on: bool| {
            let mut snap = a_snapshot(ImportStep::Scope);
            snap.import_duplicates = on;
            mail_import_elements(&state_with(snap))
                .into_iter()
                .find(|e| e.id == "mail-import-scope-import-duplicates-toggle")
                .expect("the toggle paints on Scope")
        };
        let state = |e: &Element| {
            e.attrs
                .iter()
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v.clone())
        };
        assert!(!a_snapshot(ImportStep::Scope).import_duplicates);
        assert_eq!(state(&toggle(false)).as_deref(), Some("off"));
        assert_eq!(state(&toggle(true)).as_deref(), Some("on"));
        for on in [false, true] {
            assert_eq!(toggle(on).text, t::SCOPE_IMPORT_DUPLICATES_LABEL);
        }
    }

    /// Each step paints only its OWN ids — the conditional-by-step contract.
    #[test]
    fn each_step_paints_only_its_own_ids() {
        let source = ids(&mail_import_elements(&state_with(a_snapshot(
            ImportStep::Source,
        ))));
        assert!(source.iter().any(|id| id == "mail-import-source-picker"));
        assert!(!source.iter().any(|id| id == "mail-import-start-button"));
        assert!(!source.iter().any(|id| id == "mail-import-scope-date-from"));

        let scope = ids(&mail_import_elements(&state_with(a_snapshot(
            ImportStep::Scope,
        ))));
        assert!(scope.iter().any(|id| id == "mail-import-scope-date-from"));
        assert!(!scope.iter().any(|id| id == "mail-import-source-picker"));

        let done = ids(&mail_import_elements(&state_with(a_snapshot(
            ImportStep::Done,
        ))));
        assert!(
            done.iter()
                .any(|id| id == "mail-import-view-imported-button")
        );
        assert!(!done.iter().any(|id| id == "mail-import-pause-button"));
    }

    /// A failed import is painted on its Progress step with the reason as the
    /// error log's last line; Pause / Resume / Cancel are not offered, nor the
    /// resume credential, and its one control is Dismiss
    /// (`mailbox-migration.md` § A failed import is a record the wizard shows
    /// until the user moves on).
    #[test]
    fn a_failed_import_offers_dismiss_and_none_of_the_three_controls() {
        let mut snap = a_snapshot(ImportStep::Progress);
        snap.session_state = Some(ImportSessionState::Errored);
        snap.resume_needs_credentials = true;
        snap.error_log = vec!["INBOX uid 2: dedup".into(), "The import stopped.".into()];
        let els = mail_import_elements(&state_with(snap));
        let painted = ids(&els);
        for id in [
            "mail-import-pause-button",
            "mail-import-resume-button",
            "mail-import-cancel-button",
            "mail-import-source-app-password",
        ] {
            assert!(!painted.iter().any(|p| p == id), "{id} on a failed import");
        }
        assert_eq!(
            text_of(&els, "mail-import-dismiss-button", 0),
            t::DISMISS_FAILED_BUTTON
        );
        let dismiss = els
            .iter()
            .find(|e| e.id == "mail-import-dismiss-button")
            .unwrap();
        assert!(matches!(
            dismiss.role,
            Role::Button(Gesture::Settings(Action::MailImportDismiss))
        ));
        assert_eq!(
            text_of(&els, "mail-import-error-log", 0),
            "INBOX uid 2: dedup\nThe import stopped."
        );

        // A live import keeps its three controls and paints no Dismiss.
        let mut live = a_snapshot(ImportStep::Progress);
        live.session_state = Some(ImportSessionState::Running);
        let painted = ids(&mail_import_elements(&state_with(live)));
        assert!(painted.iter().any(|p| p == "mail-import-pause-button"));
        assert!(!painted.iter().any(|p| p == "mail-import-dismiss-button"));
    }

    /// The push re-read is page-gated (`StaleSurfaces::mail_import`): only the
    /// open Import sub-page, and only once a machine exists, produces an op.
    #[test]
    fn the_import_push_re_reads_only_the_open_import_page() {
        let mut state = state_with(a_snapshot(ImportStep::Progress));
        assert!(
            crate::settings::mail_import_resync_op(&state).is_none(),
            "no machine pre-login, no op"
        );
        let nest = fauna_client::NestClient::new(
            "http://127.0.0.1:9".to_string(),
            fauna_core::identity::ActorKeypair::from_secret([7u8; 32]),
        );
        state.mail_import.machine =
            Some(Arc::new(crate::mail_glue::build_mail_import_machine(nest)));
        assert!(matches!(
            crate::settings::mail_import_resync_op(&state),
            Some(crate::settings::Op::ResyncMailImport { .. })
        ));
        state.sub = SubPage::MailSpam;
        assert!(
            crate::settings::mail_import_resync_op(&state).is_none(),
            "another sub-page shown"
        );
    }

    /// A session adopted after a restart asks for the provider's own
    /// credential on the Progress step (§ Resume protocol step 2) — and only
    /// then: the app that started the import paints none.
    #[test]
    fn an_adopted_session_asks_for_its_credential_on_the_progress_step() {
        let progress_ids = |kind: ImportSourceKind, needs: bool| {
            let mut snap = a_snapshot(ImportStep::Progress);
            snap.source_kind = kind;
            snap.resume_needs_credentials = needs;
            ids(&mail_import_elements(&state_with(snap)))
        };
        let has = |ids: &[String], id: &str| ids.iter().any(|i| i == id);

        let started_here = progress_ids(ImportSourceKind::Generic, false);
        assert!(!has(&started_here, "mail-import-source-password"));
        assert!(!has(&started_here, "mail-import-source-app-password"));

        let gmail = progress_ids(ImportSourceKind::Gmail, true);
        assert!(has(&gmail, "mail-import-source-app-password"));
        assert!(!has(&gmail, "mail-import-source-password"));
        assert!(has(&gmail, "mail-import-resume-button"));

        let generic = progress_ids(ImportSourceKind::Generic, true);
        for id in [
            "mail-import-source-port",
            "mail-import-source-tls-mode",
            "mail-import-source-password",
        ] {
            assert!(has(&generic, id), "missing {id}: {generic:?}");
        }
        assert!(
            !has(&generic, "mail-import-source-host"),
            "the host is the session's, not the person's to change on a resume"
        );

        let outlook = progress_ids(ImportSourceKind::Outlook, true);
        assert!(has(&outlook, "mail-import-source-oauth-button"));
        assert!(!has(&outlook, "mail-import-source-password"));

        // The adopted Outlook session's sign-in shows its code there too.
        let mut signing_in = outlook_waiting();
        signing_in.step = ImportStep::Progress;
        signing_in.resume_needs_credentials = true;
        let els = mail_import_elements(&state_with(signing_in));
        assert_eq!(
            text_of(&els, "mail-import-source-oauth-code", 0),
            t::microsoft_code_fmt("https://microsoft.example/devicelogin", "FAUNA-42"),
        );
    }

    /// Pre-hydrate the wizard still opens on a usable Source step with the
    /// ratified default provider (Gmail) — the `mail_export.rs` precedent.
    #[test]
    fn pre_hydrate_the_wizard_still_opens_on_a_usable_source_step() {
        let state = SettingsState {
            sub: SubPage::MailImport,
            ..Default::default()
        };
        let els = mail_import_elements(&state);
        assert!(ids(&els).contains(&"mail-import-source-picker".to_string()));
        assert_eq!(
            text_of(&els, "mail-import-source-picker", 0),
            source_kind_label(ImportSourceKind::Gmail),
        );
        // Gmail's fields, not Generic's.
        assert!(ids(&els).contains(&"mail-import-source-app-password".to_string()));
        assert!(!ids(&els).contains(&"mail-import-source-host".to_string()));
    }

    /// The source picker's options and painted value both come from the
    /// SHARED `import_source_kind_label` map, and every label resolves back to
    /// the kind it names.
    #[test]
    fn the_source_picker_round_trips_the_shared_label() {
        let mut snap = a_snapshot(ImportStep::Source);
        snap.source_kind = ImportSourceKind::ICloud;
        let els = mail_import_elements(&state_with(snap));
        assert_eq!(
            text_of(&els, "mail-import-source-picker", 0),
            source_kind_label(ImportSourceKind::ICloud),
        );
        for kind in SOURCE_KINDS {
            assert_eq!(source_kind_for_label(&source_kind_label(kind)), kind);
        }
    }

    /// Per-provider field visibility: Gmail/iCloud show app-password, never
    /// host/port; Outlook shows the address and Sign in with Microsoft — no
    /// password, no host, no Connect (`mailbox-migration.md` § Microsoft
    /// sign-in: the fallback is withdrawn); Generic shows the IMAP fields,
    /// never app-password or OAuth.
    #[test]
    fn per_provider_source_fields_match_the_visibility_table() {
        let painted_for = |kind: ImportSourceKind| {
            let mut snap = a_snapshot(ImportStep::Source);
            snap.source_kind = kind;
            ids(&mail_import_elements(&state_with(snap)))
        };

        let gmail = painted_for(ImportSourceKind::Gmail);
        assert!(gmail.contains(&"mail-import-source-app-password".to_string()));
        assert!(gmail.contains(&"mail-import-source-username".to_string()));
        assert!(!gmail.contains(&"mail-import-source-host".to_string()));
        assert!(!gmail.contains(&"mail-import-source-oauth-button".to_string()));

        let icloud = painted_for(ImportSourceKind::ICloud);
        assert!(icloud.contains(&"mail-import-source-app-password".to_string()));
        assert!(!icloud.contains(&"mail-import-source-port".to_string()));

        let outlook = painted_for(ImportSourceKind::Outlook);
        assert!(outlook.contains(&"mail-import-source-oauth-button".to_string()));
        assert!(outlook.contains(&"mail-import-source-username".to_string()));
        for absent in [
            "mail-import-source-host",
            "mail-import-source-port",
            "mail-import-source-tls-mode",
            "mail-import-source-password",
            "mail-import-source-app-password",
            "mail-import-connect-button",
            // No code before the button was pressed.
            "mail-import-source-oauth-code",
        ] {
            assert!(
                !outlook.contains(&absent.to_string()),
                "{absent}: {outlook:?}"
            );
        }

        let generic = painted_for(ImportSourceKind::Generic);
        assert!(generic.contains(&"mail-import-source-host".to_string()));
        assert!(generic.contains(&"mail-import-source-tls-mode".to_string()));
        assert!(!generic.contains(&"mail-import-source-oauth-button".to_string()));
        assert!(!generic.contains(&"mail-import-source-app-password".to_string()));
    }

    fn oauth_button(els: &[Element]) -> &Element {
        els.iter()
            .find(|e| e.id == "mail-import-source-oauth-button")
            .expect("oauth button painted")
    }

    /// Until this platform can sign in (the project's application ID still
    /// empty) the button paints inert beside the unavailable sentence — never
    /// a failing request (§ Microsoft sign-in, *Whose registration*).
    #[test]
    fn the_oauth_button_is_inert_until_sign_in_is_available() {
        let mut snap = a_snapshot(ImportStep::Source);
        snap.source_kind = ImportSourceKind::Outlook;
        snap.microsoft_signin_available = false;
        let els = mail_import_elements(&state_with(snap.clone()));
        assert!(!oauth_button(&els).enabled);
        assert!(els.iter().any(|e| e.text == t::MICROSOFT_UNAVAILABLE));

        snap.microsoft_signin_available = true;
        let els = mail_import_elements(&state_with(snap));
        let button = oauth_button(&els);
        assert!(button.enabled);
        assert!(matches!(
            button.gesture(),
            Some(Gesture::Settings(Action::MailImportOAuthStart))
        ));
        assert!(!els.iter().any(|e| e.text == t::MICROSOFT_UNAVAILABLE));
    }

    /// Once Microsoft answered, the code and its address paint on
    /// `mail-import-source-oauth-code` (each alone as an attribute, for a
    /// driver to read) with Open in browser beside it, and the button stays
    /// inert while the sign-in is in flight — and the page's tick runs.
    #[test]
    fn a_waiting_sign_in_paints_the_code_and_its_address() {
        let state = state_with(outlook_waiting());
        let els = mail_import_elements(&state);
        let code = els
            .iter()
            .find(|e| e.id == "mail-import-source-oauth-code")
            .expect("code painted");
        assert_eq!(
            code.text,
            t::microsoft_code_fmt("https://microsoft.example/devicelogin", "FAUNA-42")
        );
        assert_eq!(
            code.attrs
                .iter()
                .find(|(k, _)| k == "code")
                .map(|(_, v)| v.as_str()),
            Some("FAUNA-42")
        );
        assert_eq!(
            code.attrs
                .iter()
                .find(|(k, _)| k == "url")
                .map(|(_, v)| v.as_str()),
            Some("https://microsoft.example/devicelogin")
        );
        assert!(els.iter().any(|e| e.text == t::MICROSOFT_OPEN_BROWSER
            && matches!(
                e.gesture(),
                Some(Gesture::Settings(Action::MailImportOpenMicrosoftSignIn))
            )));
        assert!(!oauth_button(&els).enabled, "one sign-in at a time");
        assert!(crate::settings::mail_import_progress_active(&state));
    }

    /// A sign-in that ended — declined, expired, refused — leaves no code and
    /// a live button for another try; its sentence is on `error-message`, and
    /// the tick goes quiet.
    #[test]
    fn an_ended_sign_in_offers_the_button_again() {
        let mut snap = outlook_waiting();
        if let Some(v) = snap.microsoft_signin.as_mut() {
            v.state = MicrosoftSignInState::Declined;
        }
        snap.error = Some(t::MICROSOFT_DECLINED.into());
        let state = state_with(snap);
        let els = mail_import_elements(&state);
        assert!(!ids(&els).contains(&"mail-import-source-oauth-code".to_string()));
        assert!(oauth_button(&els).enabled);
        assert!(!crate::settings::mail_import_progress_active(&state));
    }

    /// The security pin this closes: both
    /// third-party password fields must render `crate::unlock::mask`'s
    /// output, never the raw buffer — `settings.md` § Credential store's
    /// masking contract, already honoured by every other secret input in the
    /// app (unlock passphrase/confirm, the re-key modal's three inputs, the
    /// add-credential password) and by linux's twin of these same two ids.
    #[test]
    fn both_source_password_fields_render_masked() {
        let password = "hunter2-app-specific-password";

        let mut gmail_snap = a_snapshot(ImportStep::Source);
        gmail_snap.source_kind = ImportSourceKind::Gmail;
        let mut gmail_state = state_with(gmail_snap);
        gmail_state.mail_import.password_input = password.to_string();
        let app_password_text = text_of(
            &mail_import_elements(&gmail_state),
            "mail-import-source-app-password",
            0,
        );
        assert_ne!(
            app_password_text, password,
            "the app-password field must not paint the raw buffer"
        );
        assert_eq!(
            app_password_text.chars().count(),
            password.chars().count(),
            "a masked field still reveals the buffer's length, same as unlock::mask elsewhere"
        );

        let mut generic_snap = a_snapshot(ImportStep::Source);
        generic_snap.source_kind = ImportSourceKind::Generic;
        let mut generic_state = state_with(generic_snap);
        generic_state.mail_import.password_input = password.to_string();
        let password_text = text_of(
            &mail_import_elements(&generic_state),
            "mail-import-source-password",
            0,
        );
        assert_ne!(
            password_text, password,
            "the IMAP-fallback password field must not paint the raw buffer"
        );
        assert_eq!(
            password_text.chars().count(),
            password.chars().count(),
            "a masked field still reveals the buffer's length, same as unlock::mask elsewhere"
        );
    }

    /// `connect_actions` commits only the fields the kind actually shows, then
    /// Connect — one multi-action dispatch. Gmail never sends `SetHost`/
    /// `SetPort` (there is nothing to commit — the field isn't painted).
    #[test]
    fn connect_actions_commit_only_the_shown_fields() {
        let mut s = MailImportState {
            username_input: "user@gmail.com".into(),
            password_input: "app-pw".into(),
            host_input: "should-not-be-sent".into(),
            ..MailImportState::default()
        };
        let actions = s.connect_actions(ImportSourceKind::Gmail);
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, MailImportAction::SetHost { .. })),
            "Gmail must not commit a host — the field is not painted"
        );
        assert_eq!(actions.last(), Some(&MailImportAction::Connect));

        s.host_input = "imap.example.com".into();
        s.port_input = "993".into();
        let actions = s.connect_actions(ImportSourceKind::Generic);
        assert!(actions.contains(&MailImportAction::SetHost {
            value: "imap.example.com".into()
        }));
        assert!(actions.contains(&MailImportAction::SetPort { value: 993 }));
    }

    /// An unparseable port is simply not committed — the machine keeps its
    /// prior value rather than the dispatch erroring.
    #[test]
    fn an_unparseable_port_is_not_committed() {
        let s = MailImportState {
            host_input: "imap.example.com".into(),
            port_input: "not a port".into(),
            ..MailImportState::default()
        };
        let actions = s.connect_actions(ImportSourceKind::Generic);
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, MailImportAction::SetPort { .. }))
        );
    }

    /// Advancing off the Scope step commits BOTH the date and max-size drafts
    /// before Next, in one awaited op — the `mail_export.rs`
    /// `advancing_off_scope_commits_both_dates_before_next` shape. The max-size
    /// MB buffer converts to bytes.
    #[test]
    fn advancing_off_scope_commits_both_drafts_before_next() {
        let s = MailImportState {
            date_from_input: "  2026-01-01  ".into(),
            max_size_input: "10".into(),
            ..MailImportState::default()
        };
        assert_eq!(
            s.scope_next_actions(),
            vec![
                MailImportAction::SetDateFrom {
                    value: "2026-01-01".into()
                },
                MailImportAction::SetMaxSizeBytes {
                    value: 10 * 1024 * 1024
                },
                MailImportAction::Next,
            ]
        );
    }

    /// An unparseable max-size buffer falls back to the machine's own 50 MiB
    /// default rather than sending a stale or zero value.
    #[test]
    fn an_unparseable_max_size_falls_back_to_the_default() {
        let s = MailImportState {
            max_size_input: "not a number".into(),
            ..MailImportState::default()
        };
        assert_eq!(
            s.scope_next_actions()[1],
            MailImportAction::SetMaxSizeBytes {
                value: 50 * 1024 * 1024
            }
        );
    }

    /// A fresh visit disarms a primed cancel and drops every draft buffer.
    #[test]
    fn a_fresh_visit_disarms_cancel_and_drops_every_draft() {
        let mut s = MailImportState {
            cancel_armed: true,
            host_input: "x".into(),
            port_input: "1".into(),
            username_input: "u".into(),
            password_input: "p".into(),
            date_from_input: "2026-01-01".into(),
            max_size_input: "5".into(),
            ..MailImportState::default()
        };
        s.reset_form();
        assert!(!s.cancel_armed);
        assert!(s.host_input.is_empty());
        assert!(s.port_input.is_empty());
        assert!(s.username_input.is_empty());
        assert!(s.password_input.is_empty());
        assert!(s.date_from_input.is_empty());
        assert_eq!(s.max_size_input, DEFAULT_MAX_SIZE_MB);
    }

    /// The confirm summary counts only SELECTED mailboxes and sums their
    /// message-count estimates, through the shared source-kind label.
    #[test]
    fn the_confirm_summary_counts_only_selected_mailboxes() {
        let mut snap = a_snapshot(ImportStep::Confirm);
        snap.source_kind = ImportSourceKind::Outlook;
        snap.mailboxes = vec![
            SourceMailboxOption {
                name: "INBOX".into(),
                selected: true,
                message_count: 100,
                byte_total: None,
            },
            SourceMailboxOption {
                name: "Sent".into(),
                selected: true,
                message_count: 20,
                byte_total: None,
            },
            SourceMailboxOption {
                name: "Trash".into(),
                selected: false,
                message_count: 5,
                byte_total: None,
            },
        ];
        let els = mail_import_elements(&state_with(snap));
        assert_eq!(
            text_of(&els, "mail-import-confirm-summary", 0),
            t::confirm_summary_fmt(&source_kind_label(ImportSourceKind::Outlook), "2", "120"),
        );
    }

    /// § Wizard steps step 4: once every selected mailbox is sized, the
    /// summary also says roughly how much data is coming, through the shared
    /// byte formatter; one unsized mailbox drops the size, never the count.
    #[test]
    fn the_confirm_summary_names_the_size_once_every_selected_mailbox_has_one() {
        let mut snap = a_snapshot(ImportStep::Confirm);
        snap.mailboxes = vec![
            SourceMailboxOption {
                name: "INBOX".into(),
                selected: true,
                message_count: 100,
                byte_total: Some(3 * 1024 * 1024),
            },
            SourceMailboxOption {
                name: "Sent".into(),
                selected: true,
                message_count: 20,
                byte_total: Some(1024 * 1024),
            },
        ];
        let size = crate::wizard::localized(&fauna_core::format::byte_size(4 * 1024 * 1024));
        let els = mail_import_elements(&state_with(snap.clone()));
        let summary = text_of(&els, "mail-import-confirm-summary", 0);
        assert_eq!(
            summary,
            t::confirm_summary_sized_fmt(
                &source_kind_label(ImportSourceKind::Gmail),
                "2",
                "120",
                &size
            ),
        );
        assert!(summary.contains("120 messages · about"), "{summary}");

        snap.mailboxes[1].byte_total = None;
        let els = mail_import_elements(&state_with(snap));
        assert_eq!(
            text_of(&els, "mail-import-confirm-summary", 0),
            t::confirm_summary_fmt(&source_kind_label(ImportSourceKind::Gmail), "2", "120"),
        );
    }

    /// The progress bar reads the SHARED `quota_fraction`, zero-guarded.
    #[test]
    fn the_progress_bar_uses_the_shared_quota_fraction() {
        let mut snap = a_snapshot(ImportStep::Progress);
        snap.imported_count = 3;
        snap.total_count = 4;
        let els = mail_import_elements(&state_with(snap));
        assert_eq!(text_of(&els, "mail-import-progress-bar", 0), "75%");

        let els = mail_import_elements(&state_with(a_snapshot(ImportStep::Progress)));
        assert_eq!(text_of(&els, "mail-import-progress-bar", 0), "0%");
    }

    /// The error log renders the snapshot's `error_log` lines, joined —
    /// proving the field is genuinely wired (the module docs note `run_import`
    /// populates it, correcting the struct's own stale doc comment).
    #[test]
    fn the_error_log_renders_the_snapshots_lines() {
        let mut snap = a_snapshot(ImportStep::Progress);
        snap.error_log = vec!["INBOX uid 2: parse fail".into(), "Sent: quota".into()];
        let els = mail_import_elements(&state_with(snap));
        assert_eq!(
            text_of(&els, "mail-import-error-log", 0),
            "INBOX uid 2: parse fail\nSent: quota"
        );
    }

    /// One row per mailbox the run takes (the machine's `mailbox_progress`),
    /// every leaf present, painted FLAT, its progress through the shared
    /// formatter.
    #[test]
    fn mailbox_progress_rows_paint_flat_with_each_mailboxs_own_counts() {
        let mut snap = a_snapshot(ImportStep::Progress);
        snap.mailbox_progress = vec![
            ImportMailboxProgress {
                name: "INBOX".into(),
                imported: 40,
                skipped: 1,
                errored: 1,
                total: 42,
            },
            ImportMailboxProgress {
                name: "Archive".into(),
                imported: 0,
                skipped: 0,
                errored: 0,
                total: 7,
            },
        ];
        let els = mail_import_elements(&state_with(snap));
        for id in [
            "mail-import-mailbox-progress-list-item",
            "mail-import-mailbox-progress-list-item-name",
            "mail-import-mailbox-progress-list-item-progress",
        ] {
            assert_eq!(
                els.iter().filter(|e| e.id == id).count(),
                2,
                "one {id} per mailbox the run takes"
            );
        }
        assert_eq!(
            text_of(&els, "mail-import-mailbox-progress-list-item-name", 1),
            "Archive"
        );
        assert_eq!(
            text_of(&els, "mail-import-mailbox-progress-list-item-progress", 0),
            t::progress_row_counts_fmt("40", "42", "1", "1")
        );
        assert_eq!(
            text_of(&els, "mail-import-mailbox-progress-list-item-progress", 1),
            t::progress_row_counts_fmt("0", "7", "0", "0")
        );
        assert!(
            els.iter().all(|e| e.path.is_empty()),
            "every leaf on this page paints flat"
        );
    }

    /// The cancel button relabels once armed.
    #[test]
    fn the_cancel_button_relabels_when_armed() {
        let mut state = state_with(a_snapshot(ImportStep::Progress));
        let unarmed = text_of(
            &mail_import_elements(&state),
            "mail-import-cancel-button",
            0,
        );
        state.mail_import.cancel_armed = true;
        let armed = text_of(
            &mail_import_elements(&state),
            "mail-import-cancel-button",
            0,
        );
        assert_ne!(unarmed, armed);
        assert_eq!(armed, fauna_i18n::strings::common::CONFIRM_Q);
    }

    /// The restart offer (`mailbox-migration.md` § Resume protocol step 4)
    /// is painted only while the machine holds a rebuilt mailbox, and its
    /// press is the restart gesture.
    #[test]
    fn the_restart_button_is_offered_only_for_a_rebuilt_mailbox() {
        let plain = mail_import_elements(&state_with(a_snapshot(ImportStep::Progress)));
        assert!(!ids(&plain).contains(&"mail-import-restart-button".to_string()));

        let mut snap = a_snapshot(ImportStep::Progress);
        snap.rebuilt_mailboxes = vec!["INBOX".into()];
        let els = mail_import_elements(&state_with(snap));
        let button = els
            .iter()
            .find(|e| e.id == "mail-import-restart-button")
            .expect("restart offered");
        assert_eq!(button.text, t::RESTART_BUTTON);
        assert!(matches!(
            button.role,
            crate::element::Role::Button(Gesture::Settings(Action::MailImportRestart))
        ));
    }

    /// The forget control is offered on the Source step, for every provider,
    /// only while the account remembers a source (§ Credential handling).
    #[test]
    fn the_forget_button_is_offered_only_while_a_source_is_remembered() {
        let plain = mail_import_elements(&state_with(a_snapshot(ImportStep::Source)));
        assert!(!ids(&plain).contains(&"mail-import-source-forget-button".to_string()));

        for kind in SOURCE_KINDS {
            let mut snap = a_snapshot(ImportStep::Source);
            snap.source_kind = kind;
            snap.source_remembered = true;
            let els = mail_import_elements(&state_with(snap));
            let button = els
                .iter()
                .find(|e| e.id == "mail-import-source-forget-button")
                .unwrap_or_else(|| panic!("forget offered for {kind:?}"));
            assert_eq!(button.text, t::SOURCE_FORGET_BUTTON);
            assert!(matches!(
                button.role,
                crate::element::Role::Button(Gesture::Settings(Action::MailImportForgetSource))
            ));
        }
    }

    /// View imported navigates to Conversations; Review skipped toggles the
    /// skip log onto the Done screen instead (§ Wizard steps step 6).
    #[test]
    fn the_done_step_buttons_go_to_conversations_and_the_skip_log() {
        let els = mail_import_elements(&state_with(a_snapshot(ImportStep::Done)));
        let role = |id: &str| {
            els.iter()
                .find(|e| e.id == id)
                .expect("button painted")
                .role
                .clone()
        };
        assert!(matches!(
            role("mail-import-view-imported-button"),
            crate::element::Role::Button(Gesture::Nav(Page::Conversations))
        ));
        assert!(matches!(
            role("mail-import-review-skipped-button"),
            crate::element::Role::Button(Gesture::Settings(Action::MailImportToggleSkipped))
        ));
    }

    /// The skip log stays off a Done card until Review skipped reveals it,
    /// then lists every line the run recorded — a clean import's card stays
    /// uncluttered.
    #[test]
    fn review_skipped_reveals_the_runs_skip_log_on_the_done_screen() {
        let mut snap = a_snapshot(ImportStep::Done);
        snap.error_log = vec![
            "INBOX uid 2: too large (4096 bytes, over the 1024-byte limit)".into(),
            "Gone: mailbox not found".into(),
        ];
        let mut state = state_with(snap);
        let hidden = mail_import_elements(&state);
        assert!(!ids(&hidden).contains(&"mail-import-error-log".to_string()));

        state.mail_import.show_skipped = true;
        let shown = mail_import_elements(&state);
        assert_eq!(
            text_of(&shown, "mail-import-error-log", 0),
            "INBOX uid 2: too large (4096 bytes, over the 1024-byte limit)\nGone: mailbox not found"
        );
    }

    /// A fresh visit hides the log again, like the armed cancel.
    #[test]
    fn a_fresh_visit_hides_the_skip_log_again() {
        let mut s = MailImportState {
            show_skipped: true,
            ..MailImportState::default()
        };
        s.reset_form();
        assert!(!s.show_skipped);
    }

    /// The done summary renders through the generated i18n template.
    #[test]
    fn the_done_summary_renders_through_the_i18n_template() {
        let mut snap = a_snapshot(ImportStep::Done);
        snap.imported_count = 10;
        snap.skipped_count = 1;
        snap.errored_count = 2;
        let els = mail_import_elements(&state_with(snap));
        assert_eq!(
            text_of(&els, "mail-import-done-summary", 0),
            "10 imported · 1 skipped · 2 errored"
        );
        assert_eq!(
            text_of(&els, "mail-import-done-summary", 0),
            t::done_summary_fmt("10", "1", "2"),
        );
    }

    /// § Wizard steps steps 5–6: a stopped run's progress line carries its
    /// elapsed time and no estimate, and the Done card adds how long the
    /// import took — both off the session row's clock.
    #[test]
    fn the_progress_and_done_summaries_carry_the_runs_time() {
        let mut snap = a_snapshot(ImportStep::Progress);
        snap.session_state = Some(fauna_client_mail_settings::ImportSessionState::Paused);
        snap.imported_count = 4;
        snap.total_count = 9;
        snap.started_at = 1_000;
        snap.last_progress_at = 1_000 + 5 * 60;
        let els = mail_import_elements(&state_with(snap));
        assert_eq!(
            text_of(&els, "mail-import-progress-summary", 0),
            t::progress_elapsed_fmt(&t::progress_summary_fmt("4", "9", "0", "0"), "5m"),
        );

        let mut snap = a_snapshot(ImportStep::Done);
        snap.session_state = Some(fauna_client_mail_settings::ImportSessionState::Completed);
        snap.imported_count = 10;
        snap.skipped_count = 1;
        snap.errored_count = 2;
        snap.started_at = 1_000;
        snap.last_progress_at = 1_000 + 2 * 3_600;
        let els = mail_import_elements(&state_with(snap));
        assert_eq!(
            text_of(&els, "mail-import-done-summary", 0),
            "10 imported · 1 skipped · 2 errored · took 2h 0m"
        );
    }

    /// § Failure handling → *Nest-side errors*: the full-mailbox pause paints
    /// the notice and Skip the rest beside Resume and Cancel; any other pause
    /// does not.
    #[test]
    fn a_full_mailbox_pause_paints_the_notice_and_skip_the_rest() {
        let ids_of =
            |els: &[Element]| -> Vec<String> { els.iter().map(|e| e.id.to_string()).collect() };
        let mut snap = a_snapshot(ImportStep::Progress);
        snap.session_state = Some(ImportSessionState::Paused);
        snap.pause_reason = Some(ImportPauseReason::MailboxFull);
        let els = mail_import_elements(&state_with(snap.clone()));
        assert_eq!(
            text_of(&els, "mail-import-quota-notice", 0),
            t::QUOTA_NOTICE
        );
        let painted = ids_of(&els);
        for id in [
            "mail-import-quota-skip-rest-button",
            "mail-import-resume-button",
            "mail-import-cancel-button",
        ] {
            assert!(painted.iter().any(|p| p == id), "{id} missing: {painted:?}");
        }

        snap.pause_reason = Some(ImportPauseReason::ConnectionLost);
        let painted = ids_of(&mail_import_elements(&state_with(snap)));
        assert!(
            !painted.iter().any(|p| p.starts_with("mail-import-quota-")),
            "{painted:?}"
        );
    }
}
