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
//! - **Outlook** paints the OAuth button **and** the IMAP-fallback fields
//!   (host/port/tls-mode/username/password) — ui.yaml's own
//!   "(+ Outlook fallback)" annotations on those four ids is what settles this
//!   (the doc's step 1: "OAuth … **or** IMAP user/password fallback").
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
//! are not backed by any `MailImportAction` — the shared machine never wired
//! a "view inbox" / "skip log" RPC or action. Both use the generic
//! `Gesture::Nav(Page::Conversations)` (mail lives on the Conversations page)
//! — a real navigation, not a stub, but "Review skipped" cannot deep-link to a
//! skip-log page that exists nowhere in the app yet (the error log lives
//! inline on the Progress screen instead). A known, proportionate scope limit
//! — mirrors the row's own "Not in scope" list for Outlook OAuth / the web
//! transport / restart-survivable resume.
//!
//! # The Outlook OAuth button is disabled
//!
//! `mail-import-source-oauth-button` paints (ui.yaml requires the element
//! exist) but is never actuable (`enabled: false`) — no app wires the
//! Microsoft Graph dance yet (`mailbox-migration.md`'s own "Not in scope"
//! list). The IMAP fallback fields beside it are the real, working path for
//! an Outlook account today.

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_client_mail_settings::{
    DEFAULT_MAX_SIZE_MB, ImportSourceKind, ImportStep, ImportTlsMode, MailImportAction,
    MailImportMachine, MailImportSnapshot, SOURCE_KINDS, TLS_MODES, import_source_kind_label,
    import_tls_mode_label,
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
    /// Step 1 (Generic/Outlook-fallback) — local drafts, committed by
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
        }
    }
}

impl MailImportState {
    /// Build the page's shared machine from the session's WS handle. No
    /// keypair: the import RPCs are user-tier, so the nest derives the owning
    /// actor from the authenticated caller (the Export reasoning).
    pub(super) fn build(nest: Arc<fauna_client::NestClient>) -> Self {
        Self {
            machine: Some(Arc::new(crate::mail_glue::build_mail_import_machine(nest))),
            ..Self::default()
        }
    }

    /// Drop the page-local drafts on a fresh visit: the armed cancel (the
    /// `MailExportState::reset_form` shape) and every Source/Scope buffer,
    /// which would otherwise paint stale credentials or a scope the machine no
    /// longer holds.
    pub(super) fn reset_form(&mut self) {
        self.cancel_armed = false;
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
        ImportStep::Done => els.extend(done_step_elements(snapshot)),
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
            els.push(
                Element::input(
                    ids::MAIL_IMPORT_SOURCE_USERNAME,
                    e.username_input.clone(),
                    Field::Settings(SettingsField::MailImportUsername),
                )
                .labelled(t::SOURCE_USERNAME_PLACEHOLDER),
            );
            els.push(
                Element::input(
                    ids::MAIL_IMPORT_SOURCE_APP_PASSWORD,
                    crate::unlock::mask(e.password_input.as_str()),
                    Field::Settings(SettingsField::MailImportPassword),
                )
                .labelled(t::SOURCE_APP_PASSWORD_LABEL),
            );
            els.push(Element::chrome(if kind == ImportSourceKind::Gmail {
                t::SOURCE_APP_PASSWORD_HELP_GMAIL
            } else {
                t::SOURCE_APP_PASSWORD_HELP_ICLOUD
            }));
        }
        ImportSourceKind::Outlook => {
            els.push(Element::gesture_button(
                ids::MAIL_IMPORT_SOURCE_OAUTH_BUTTON,
                t::SOURCE_OAUTH_BUTTON,
                // Disabled — see the module docs. `MailImportOAuthStart` is a
                // deliberate no-op, defense in depth if ever actuated anyway.
                false,
                Gesture::Settings(Action::MailImportOAuthStart),
            ));
            els.extend(imap_fallback_fields(e, tls_mode));
        }
        ImportSourceKind::Generic => {
            els.extend(imap_fallback_fields(e, tls_mode));
        }
    }

    els.push(Element::gesture_button(
        ids::MAIL_IMPORT_CONNECT_BUTTON,
        t::CONNECT_BUTTON,
        true,
        Gesture::Settings(Action::MailImportConnect),
    ));
    els
}

/// The host/port/tls-mode/username/password field group — Generic's required
/// set, reused as Outlook's IMAP fallback (ui.yaml's "(+ Outlook fallback)"
/// annotation on these four ids).
fn imap_fallback_fields(e: &MailImportState, tls_mode: ImportTlsMode) -> Vec<Element> {
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
/// buffer, and the mapping info line.
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
/// The summary states what IS known from the scope selection: the source, the
/// selected-mailbox count, and their summed `message_count` estimate
/// (`mailbox-migration.md` § Wizard steps step 4: "total messages … the
/// client enumerates the source" — no estimated-bytes/wall-clock field exists
/// on the snapshot, so neither is claimed here).
fn confirm_step_elements(snapshot: Option<&MailImportSnapshot>) -> Vec<Element> {
    let (source, selected, messages) = snapshot
        .map(|s| {
            let selected = s.mailboxes.iter().filter(|m| m.selected).count();
            let messages: u64 = s
                .mailboxes
                .iter()
                .filter(|m| m.selected)
                .map(|m| u64::from(m.message_count))
                .sum();
            (source_kind_label(s.source_kind), selected, messages)
        })
        .unwrap_or((source_kind_label(ImportSourceKind::Gmail), 0, 0));
    vec![
        Element::chrome(t::CONFIRM_TITLE),
        Element::label(
            ids::MAIL_IMPORT_CONFIRM_SUMMARY,
            t::confirm_summary_fmt(&source, &selected.to_string(), &messages.to_string()),
        ),
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
            t::progress_summary_fmt(
                &imported.to_string(),
                &total.to_string(),
                &skipped.to_string(),
                &errored.to_string(),
            ),
        ),
        Element::label(
            ids::MAIL_IMPORT_PROGRESS_BAR,
            format!("{:.0}%", fraction * 100.0),
        )
        .attr("fraction", format!("{fraction:.4}")),
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
        // Two-click inline confirm: the relabel is the gate's only visible
        // affordance, the `mail_export.rs` shape.
        Element::gesture_button(
            ids::MAIL_IMPORT_CANCEL_BUTTON,
            if e.cancel_armed {
                fauna_i18n::strings::common::CONFIRM_Q
            } else {
                t::CANCEL_BUTTON
            },
            true,
            Gesture::Settings(Action::MailImportCancel),
        ),
        Element::label(
            ids::MAIL_IMPORT_ERROR_LOG,
            snapshot.map(|s| s.error_log.join("\n")).unwrap_or_default(),
        ),
        Element::chrome(t::ERROR_LOG_TITLE),
        Element::label(ids::MAIL_IMPORT_MAILBOX_PROGRESS_LIST, t::PROGRESS_TITLE),
    ];
    // The machine tracks only GLOBAL imported/skipped/errored counts, not a
    // per-mailbox breakdown (`import.rs::MailImportSnapshot` has no
    // `mailbox_progress` field the way export's does) — so each row shows its
    // planned message count, not a live per-mailbox fraction. A known,
    // accurate-to-what-exists simplification, not a gap this page introduces.
    for mailbox in snapshot.map(|s| s.mailboxes.as_slice()).unwrap_or(&[]) {
        if !mailbox.selected {
            continue;
        }
        els.extend(mailbox_progress_row_elements(mailbox));
    }
    els
}

/// One `mail-import-mailbox-progress-list-item` row. FLAT — the
/// `mail_export.rs::mailbox_progress_row_elements` shape: no `.within(..)`
/// scope, matching how `actions/mail_export.py` reads its own twin.
fn mailbox_progress_row_elements(
    mailbox: &fauna_client_mail_settings::SourceMailboxOption,
) -> Vec<Element> {
    vec![
        Element::label(
            ids::MAIL_IMPORT_MAILBOX_PROGRESS_LIST_ITEM,
            mailbox.name.clone(),
        ),
        Element::label(
            ids::MAIL_IMPORT_MAILBOX_PROGRESS_LIST_ITEM_NAME,
            mailbox.name.clone(),
        ),
        Element::label(
            ids::MAIL_IMPORT_MAILBOX_PROGRESS_LIST_ITEM_PROGRESS,
            t::progress_row_fmt(&mailbox.message_count.to_string()),
        ),
    ]
}

/// Step 6 — done. The summary card and the two deep-links (module docs: both
/// use `Gesture::Nav(Page::Conversations)`, a known scope limit for
/// "Review skipped").
fn done_step_elements(snapshot: Option<&MailImportSnapshot>) -> Vec<Element> {
    let (imported, skipped, errored) = snapshot
        .map(|s| (s.imported_count, s.skipped_count, s.errored_count))
        .unwrap_or((0, 0, 0));
    vec![
        Element::chrome(t::DONE_TITLE),
        Element::label(
            ids::MAIL_IMPORT_DONE_SUMMARY,
            t::done_summary_fmt(
                &imported.to_string(),
                &skipped.to_string(),
                &errored.to_string(),
            ),
        ),
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
            Gesture::Nav(Page::Conversations),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SubPage;
    use fauna_client_mail_settings::SourceMailboxOption;
    use fauna_client_mail_settings::testing::a_mail_import_snapshot as a_snapshot;

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
        }];
        painted.extend(ids(&mail_import_elements(&state_with(scope_snap))));
        for step in [ImportStep::Confirm, ImportStep::Progress, ImportStep::Done] {
            painted.extend(ids(&mail_import_elements(&state_with(a_snapshot(step)))));
        }
        for id in [
            "page-heading",
            // "error-message" is registered globally by ui::register_frame
            // (the module docs), never painted by mail_import_elements
            // itself — the mail_export.rs precedent omits it too.
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
            "mail-import-scope-mailbox-item",
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
    /// host/port; Outlook shows BOTH the OAuth button and the IMAP fallback
    /// fields; Generic shows the fallback fields, never app-password or OAuth.
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
        assert!(outlook.contains(&"mail-import-source-host".to_string()));
        assert!(outlook.contains(&"mail-import-source-password".to_string()));
        assert!(!outlook.contains(&"mail-import-source-app-password".to_string()));

        let generic = painted_for(ImportSourceKind::Generic);
        assert!(generic.contains(&"mail-import-source-host".to_string()));
        assert!(generic.contains(&"mail-import-source-tls-mode".to_string()));
        assert!(!generic.contains(&"mail-import-source-oauth-button".to_string()));
        assert!(!generic.contains(&"mail-import-source-app-password".to_string()));
    }

    /// The OAuth button is present but disabled — `mailbox-migration.md`'s "no
    /// app wires it" gap, honestly represented rather than hidden or faked.
    #[test]
    fn the_oauth_button_is_disabled() {
        let mut snap = a_snapshot(ImportStep::Source);
        snap.source_kind = ImportSourceKind::Outlook;
        let els = mail_import_elements(&state_with(snap));
        let button = els
            .iter()
            .find(|e| e.id == "mail-import-source-oauth-button")
            .expect("oauth button painted");
        assert!(!button.enabled, "the OAuth button must not be actuable yet");
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
            },
            SourceMailboxOption {
                name: "Sent".into(),
                selected: true,
                message_count: 20,
            },
            SourceMailboxOption {
                name: "Trash".into(),
                selected: false,
                message_count: 5,
            },
        ];
        let els = mail_import_elements(&state_with(snap));
        assert_eq!(
            text_of(&els, "mail-import-confirm-summary", 0),
            t::confirm_summary_fmt(&source_kind_label(ImportSourceKind::Outlook), "2", "120"),
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

    /// One row per SELECTED mailbox with every leaf present, painted FLAT.
    #[test]
    fn mailbox_progress_rows_paint_flat_for_selected_mailboxes_only() {
        let mut snap = a_snapshot(ImportStep::Progress);
        snap.mailboxes = vec![
            SourceMailboxOption {
                name: "INBOX".into(),
                selected: true,
                message_count: 42,
            },
            SourceMailboxOption {
                name: "Trash".into(),
                selected: false,
                message_count: 7,
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
                1,
                "one {id} — only the selected mailbox"
            );
        }
        assert_eq!(
            text_of(&els, "mail-import-mailbox-progress-list-item-name", 0),
            "INBOX"
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

    /// The done step's two buttons both navigate to Conversations — the
    /// module docs' documented scope limit, proven rather than assumed.
    #[test]
    fn the_done_step_buttons_navigate_to_conversations() {
        let els = mail_import_elements(&state_with(a_snapshot(ImportStep::Done)));
        for id in [
            "mail-import-view-imported-button",
            "mail-import-review-skipped-button",
        ] {
            let button = els.iter().find(|e| e.id == id).expect("button painted");
            assert!(matches!(
                button.role,
                crate::element::Role::Button(Gesture::Nav(Page::Conversations))
            ));
        }
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
}
