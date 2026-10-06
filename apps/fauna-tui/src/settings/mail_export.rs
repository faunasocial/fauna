//! The Settings → Mail → Export mailbox wizard (`docs/goal/behavior/mail-export.md`
//! § UX shape → § Wizard steps; `tests/e2e-unified/ui.yaml` `mail-export` page +
//! its `mail-export-mailbox-progress-list` component).
//!
//! Where a person pulls their whole mailbox out in an MUA-portable format — a
//! five-step wizard (Format → Scope → Confirm → Progress → Done) that is a dumb
//! renderer of the shared `fauna_client_mail_settings::MailExportSnapshot` and a
//! dispatcher of `MailExportAction`. The FSM (step transitions, mailbox
//! default-selection, start/pause/resume/cancel sequencing) is shared Rust
//! (priority #2/#4); tui is the **seventh and last app** to lift the page, and the
//! second direct-Rust consumer after linux (`apps/fauna-linux/src/settings/
//! mail_export.rs`) — no FFI hop, and no shared-Rust work was owed.
//!
//! # One element set, painted one step at a time
//!
//! `mail-export.md:37` ratifies "the wizard's five steps share one element set on
//! the page, shown conditionally by step". On a widget-tree app that means build
//! all, `set_visible` one (linux). On tui the painted element list **is** what the
//! driver sees, so "shown" means "painted": each step contributes only its own
//! ids, the local `folders.rs::wizard_elements` idiom. Walking all five steps is
//! therefore what proves the full ui.yaml scope — which is what
//! `every_static_ui_yaml_id_paints_across_the_five_steps` does.
//!
//! # tui is the first app where this wizard actually exports (2026-09-22)
//!
//! The page itself did not change; what changed is behind it. `mail_glue::
//! build_mail_export_machine` now gives the shared machine **key custody**, and
//! `settings/mod.rs`'s `Op::MailExportDispatch` **spawns
//! `MailExportMachine::run_export`** once a `Start` or `Resume` lands `Running`.
//! Those two are one change, never two: custody without a spawn opens a real
//! session nothing drives — a Progress screen stuck at zero holding one of the
//! user's three concurrency slots, the fake-green `Start` the track forbids
//! (`rpc_glue::build_mail_export_machine_without_key_custody`, which the other
//! six apps still build until their trickle-down). A 1-second page-scoped tick
//! (`mail_export_progress_active`) repaints Progress from the snapshot the loop
//! mutates, and `MailExportAction::Download` runs § Download flow end to end.
//!
//! - The **hydrate** failure is silent by design, matching Lists/Aliases: the
//!   machine's `hydrate()` calls `refresh()` directly and never stamps the
//!   snapshot, so an unread mailbox list paints as an empty scope step with its
//!   placeholder — "no mailboxes read yet" is not an error the user caused, and
//!   claiming one on arrival would be noise on every visit.
//! - **`Start` is where a real rejection surfaces** (an over-quota scope, a
//!   session this app holds no stream for, a superseded generation). It goes
//!   through `dispatch`, which stamps `snapshot.error` on failure, and
//!   `apply_mail_export_snapshot` bridges that onto `error-message`. Never a
//!   fake-green, never a hidden button, never a dropped command (testing.md
//!   point 11) — which is also why the Done summary names the saved path once
//!   the archive is on disk: tui raises no save dialog, so that line is the only
//!   thing that can make Download visible.
//!
//! # The encrypted-metadata scope toggle is DELETED — do not add it back
//!
//! A ratified design round (2026-07-23, user-approved: export conversion is
//! client-side unconditionally) retired the encrypted-metadata scope toggle; the
//! coordinated cross-app cleanup then deleted it entirely
//! — the ui.yaml element, the shared machine's `plaintext_mode`/
//! `include_encrypted_metadata` fields, and every other app's render leg. tui
//! never wired the axis in the first place, so nothing here changed.
//!
//! # Both lists paint FLAT, and the shared action is why
//!
//! `tests/e2e-unified/actions/mail_export.py` reads the per-mailbox progress rows
//! with a plain `driver.count("mail-export-mailbox-progress-list-item-name")` —
//! there is no `scope=` path anywhere in that file. So the row leaves paint flat
//! (the `mail_spam.rs` / `mail_aliases.rs` shape), **not** the
//! `.within("<container>", i)` shape muted-words and task-delegation use.
//! Declaring containment would put the container first in every leaf's path and
//! leave every read resolving to nothing while the page painted perfectly — the
//! `restore-history-item` bug, mirrored. Read the shared action before choosing a
//! row shape; neither flat nor scoped is this codebase's default.
//!
//! # The Next/Back buttons and the per-mailbox checkboxes are tagged, mirroring
//! # mail-import
//!
//! tui's automation registry never registers an element with an empty id
//! (`ui.rs::register_frame` skips `id.is_empty()`), so this page's Next/Back
//! (`format_step_elements`/`scope_step_elements`/`confirm_step_elements`) and
//! per-mailbox `checkbox_gesture` rows reuse the shared `wizard-next-button`/
//! `wizard-back-button` ids and an indexed `mail-export-scope-mailbox-item`,
//! user-approved 2026-08-29 — the identical shape the
//! sibling `mail-import` wizard landed 2026-08-28. `actions/mail_export.py`'s
//! `next()`/`back()`/`mailbox_names()`/`toggle_mailbox()` methods drive them.

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_client_mail_settings::{
    ExportFormat, ExportStep, MailExportAction, MailExportMachine, MailExportSnapshot,
    export_format_label,
};
use fauna_i18n::strings::mail_export as t;

use super::{Action, SettingsField, SettingsState};
use crate::element::{Element, Field, Gesture, SelectTarget};

/// The Export sub-page's state.
///
/// The machine is built once at the post-auth hook (`attach_session`), the
/// `MailListsState`/`MailSpamState` shape — construction is sync, cheap and
/// infallible (the RPC is `hydrate()`), and holding it as an `Arc` is what lets an
/// `Op` own only `Arc`s and cross a `tokio::spawn`.
#[derive(Default)]
pub(crate) struct MailExportState {
    /// The shared wizard machine. `None` pre-auth — the page then paints its
    /// static step-1 controls and its nav edge produces no `Op`, the Mail-page
    /// degradation.
    pub(super) machine: Option<Arc<MailExportMachine>>,
    /// The last rendered snapshot. `None` until the first hydrate resolves.
    pub(super) snapshot: Option<MailExportSnapshot>,
    /// The step-2 "since" buffer. A LOCAL draft (`set_field` commits nothing),
    /// pushed to the machine as `SetDateFrom` when the user advances off the
    /// Scope step — a dispatch per keystroke would be one awaited op per
    /// character.
    pub(super) date_from_input: String,
    /// The step-2 "until" buffer — [`Self::date_from_input`]'s twin.
    pub(super) date_to_input: String,
    /// Set once Cancel has been armed by a first click. Cancel aborts the run and
    /// unlinks the partial blob (`mail-export.md` § Wizard steps 4), so it gets
    /// the two-click inline gate linux gives it — tui has no modal, and ui.yaml
    /// scopes no confirm id on this page.
    pub(super) cancel_armed: bool,
}

impl MailExportState {
    /// Build the page's shared machine from the session's WS handle, identity
    /// secret and handle. The keypair is load-bearing, not ceremony: the drive
    /// loop opens every exported record under the actor's standing key set and
    /// wraps the per-session blob key under the actor's key, and `handle` names
    /// the archive's root directory (`mail_glue::build_mail_export_machine`).
    ///
    /// An undecodable secret degrades to the custody-less machine rather than
    /// to no machine: listing, resume, Cancel and Discard still work and `Start`
    /// refuses honestly, which is strictly better than a page with no surface —
    /// and better than custody without a spawn, which would be the fake-green
    /// `Start` the track forbids.
    pub(super) fn build(
        nest: Arc<fauna_client::NestClient>,
        secret_hex: &str,
        mail: Arc<dyn fauna_client_config::MailStore>,
        node_url: &str,
        handle: &str,
    ) -> Self {
        let machine = match crate::mail_glue::build_mail_export_machine(
            Arc::clone(&nest),
            secret_hex,
            mail,
            node_url,
            handle,
        ) {
            Ok(machine) => machine,
            Err(e) => {
                tracing::error!("[settings/mail_export] build machine: {e}");
                crate::mail_glue::build_mail_export_machine_without_key_custody(nest)
            }
        };
        Self {
            machine: Some(Arc::new(machine)),
            ..Self::default()
        }
    }

    /// Drop the page-local drafts on a fresh visit: the armed cancel, so a primed
    /// destructive confirm never survives a nav-away to fire against a later
    /// visit (`MailSpamState::reset_form`'s reasoning), and the date buffers,
    /// which would otherwise paint a range the machine no longer holds.
    pub(super) fn reset_form(&mut self) {
        self.cancel_armed = false;
        self.date_from_input.clear();
        self.date_to_input.clear();
    }

    /// The actions one `Next` from the Scope step must dispatch, in order: commit
    /// both date drafts, then advance. Returned as a list because tui runs **one
    /// awaited op per gesture** — firing the commits as separate background
    /// dispatches would be the fire-and-forget shape that contract forbids, and
    /// would let the agent observe Confirm before the scope it summarizes landed.
    pub(super) fn scope_next_actions(&self) -> Vec<MailExportAction> {
        vec![
            MailExportAction::SetDateFrom {
                value: self.date_from_input.trim().to_string(),
            },
            MailExportAction::SetDateTo {
                value: self.date_to_input.trim().to_string(),
            },
            MailExportAction::Next,
        ]
    }
}

/// The three format options, in `mail-export.md` § Wizard steps 1 order (mbox is
/// the default selection). The picker round-trips the **label** — the shared
/// action drives it as `select("mail-export-format-picker", label)` and linux
/// builds its DropDown from exactly these three strings, so a token split here
/// would make one app's `select` call fail against another's picker.
fn format_options() -> Vec<String> {
    [
        ExportFormat::Mbox,
        ExportFormat::MaildirPlus,
        ExportFormat::EmlZip,
    ]
    .into_iter()
    .map(format_label)
    .collect()
}

/// Resolve the SHARED `export_format_label` map through tui's i18n runtime.
///
/// `mail-export.md:15` made this the one source of truth for the
/// `ExportFormat`→`mail_export.format_*` map precisely so that the apps which had
/// each hard-coded the identical three arms could be lifted onto it. A seventh
/// hand-rolled copy is the drift that doc closed.
fn format_label(format: ExportFormat) -> String {
    crate::wizard::localized(&export_format_label(format))
}

/// Map a picker label back to its [`ExportFormat`]. An unrecognized label keeps
/// the default rather than panicking (the `ExternalMedia` picker's contract).
pub(crate) fn format_for_label(label: &str) -> ExportFormat {
    [
        ExportFormat::Mbox,
        ExportFormat::MaildirPlus,
        ExportFormat::EmlZip,
    ]
    .into_iter()
    .find(|f| format_label(*f) == label)
    .unwrap_or(ExportFormat::Mbox)
}

/// The Export sub-page's ordered element list — only the ACTIVE step's ids, per
/// the module docs.
///
/// The page's `error-message` is registered globally by
/// [`crate::ui::register_frame`] (the mail-spam/privacy precedent), so a snapshot
/// error bridges onto it through `App::errors` rather than being painted here.
pub(super) fn mail_export_elements(state: &SettingsState) -> Vec<Element> {
    let e = &state.mail_export;
    let snapshot = e.snapshot.as_ref();
    // Pre-hydrate the wizard still opens on step 1 with the default format: the
    // client-side steps do not depend on the nest, which is the whole reason this
    // page is usable against an unbuilt backend.
    let step = snapshot.map(|s| s.step).unwrap_or(ExportStep::Format);
    let format = snapshot.map(|s| s.format).unwrap_or(ExportFormat::Mbox);

    let mut els = vec![
        Element::label(ids::PAGE_HEADING, t::TITLE),
        Element::chrome(t::DESCRIPTION),
    ];

    match step {
        ExportStep::Format => els.extend(format_step_elements(format)),
        ExportStep::Scope => els.extend(scope_step_elements(e, snapshot)),
        ExportStep::Confirm => els.extend(confirm_step_elements(snapshot, format)),
        ExportStep::Progress => els.extend(progress_step_elements(e, snapshot)),
        ExportStep::Done => els.extend(done_step_elements(snapshot, format)),
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

/// Step 1 — the format picker. `Next` carries the shared `wizard-next-button`
/// id (user-approved 2026-08-29, mirroring `mail-import`'s Scope/Confirm nav).
fn format_step_elements(format: ExportFormat) -> Vec<Element> {
    vec![
        Element::chrome(t::FORMAT_TITLE),
        Element::select(
            ids::MAIL_EXPORT_FORMAT_PICKER,
            format_label(format),
            SelectTarget::MailExportFormat,
            format_options(),
        )
        .labelled(t::FORMAT_TITLE),
        Element::gesture_button(
            ids::WIZARD_NEXT_BUTTON,
            t::NEXT,
            true,
            Gesture::Settings(Action::MailExportNext),
        ),
    ]
}

/// Step 2 — scope. The mailbox multi-select, the two date buffers, and the
/// header-strip toggle.
///
/// The per-mailbox checkboxes carry the REAL, indexed `mail-export-scope-
/// mailbox-item` id (user-approved 2026-08-29, mirroring `mail-import`'s
/// `mail-import-scope-mailbox-item`, approved 2026-08-28). The container
/// (`mail-export-scope-mailboxes`) itself is still a real element the driver
/// can read, not decoration — linux paints it as a group marker for the same
/// reason.
///
/// The encrypted-metadata toggle is deliberately absent — see the module docs.
fn scope_step_elements(e: &MailExportState, snapshot: Option<&MailExportSnapshot>) -> Vec<Element> {
    let strip_headers = snapshot.is_some_and(|s| s.strip_headers);
    let mailboxes = snapshot.map(|s| s.mailboxes.as_slice()).unwrap_or(&[]);

    let mut els = vec![
        Element::chrome(t::SCOPE_TITLE),
        Element::label(ids::MAIL_EXPORT_SCOPE_MAILBOXES, t::SCOPE_MAILBOXES_LABEL),
    ];
    if mailboxes.is_empty() {
        els.push(Element::chrome(t::SCOPE_MAILBOXES_EMPTY));
    }
    for mailbox in mailboxes {
        els.push(
            Element::checkbox_gesture(
                ids::MAIL_EXPORT_SCOPE_MAILBOX_ITEM,
                mailbox.name.clone(),
                mailbox.selected,
                Gesture::Settings(Action::MailExportToggleMailbox(mailbox.name.clone())),
            )
            .attr("state", if mailbox.selected { "on" } else { "off" }),
        );
    }

    els.push(
        Element::input(
            ids::MAIL_EXPORT_SCOPE_DATE_FROM,
            e.date_from_input.clone(),
            Field::Settings(SettingsField::MailExportDateFrom),
        )
        .labelled(t::SCOPE_DATE_FROM_PLACEHOLDER),
    );
    els.push(
        Element::input(
            ids::MAIL_EXPORT_SCOPE_DATE_TO,
            e.date_to_input.clone(),
            Field::Settings(SettingsField::MailExportDateTo),
        )
        .labelled(t::SCOPE_DATE_TO_PLACEHOLDER),
    );
    els.push(
        Element::checkbox_gesture(
            ids::MAIL_EXPORT_SCOPE_STRIP_HEADERS_TOGGLE,
            t::SCOPE_STRIP_HEADERS_LABEL,
            strip_headers,
            Gesture::Settings(Action::MailExportToggleStripHeaders),
        )
        .attr("state", if strip_headers { "on" } else { "off" }),
    );
    els.push(Element::chrome(t::SCOPE_STRIP_HEADERS_SUBTITLE));
    els.push(Element::gesture_button(
        ids::WIZARD_BACK_BUTTON,
        t::BACK,
        true,
        Gesture::Settings(Action::MailExportBack),
    ));
    els.push(Element::gesture_button(
        ids::WIZARD_NEXT_BUTTON,
        t::NEXT,
        true,
        Gesture::Settings(Action::MailExportNext),
    ));
    els
}

/// Step 3 — confirmation + the durable commit.
///
/// `mail-export.md` § Wizard steps 3 wants "total messages + estimated bytes +
/// estimated wall-clock time" in the summary. The snapshot carries none of those:
/// they come from the unbuilt `start_export_session` estimate path, so the
/// summary states what IS known (format + how many mailboxes are selected) and the
/// i18n copy `confirm_pending` says the estimate is unavailable until the backend
/// lands. That is linux's `confirm_text` verbatim in content — an invented
/// estimate would be the fake-green this page exists to avoid.
fn confirm_step_elements(
    snapshot: Option<&MailExportSnapshot>,
    format: ExportFormat,
) -> Vec<Element> {
    let selected = snapshot
        .map(|s| s.mailboxes.iter().filter(|m| m.selected).count())
        .unwrap_or(0);
    vec![
        Element::chrome(t::CONFIRM_TITLE),
        Element::label(
            ids::MAIL_EXPORT_CONFIRM_SUMMARY,
            t::confirm_summary_fmt(&format_label(format), &selected.to_string()),
        ),
        Element::chrome(t::CONFIRM_PENDING),
        Element::gesture_button(
            ids::WIZARD_BACK_BUTTON,
            t::BACK,
            true,
            Gesture::Settings(Action::MailExportBack),
        ),
        Element::gesture_button(
            ids::MAIL_EXPORT_START_BUTTON,
            t::START_BUTTON,
            true,
            Gesture::Settings(Action::MailExportStart),
        ),
    ]
}

/// Step 4 — progress. Overall counters, the bar, the pause/resume/cancel
/// controls, the per-message error log, and the per-mailbox progress rows.
fn progress_step_elements(
    e: &MailExportState,
    snapshot: Option<&MailExportSnapshot>,
) -> Vec<Element> {
    let (exported, total, skipped, errored) = snapshot
        .map(|s| {
            (
                s.exported_count,
                s.total_count,
                s.skipped_count,
                s.errored_count,
            )
        })
        .unwrap_or((0, 0, 0, 0));
    // The SHARED zero-guarded, clamped ratio (`mail-export.md:17`) — the same
    // `quota_fraction` linux/android/apple call for this exact bar. A local
    // `exported / total` would be a fourth hand-rolled twin of it.
    let fraction = fauna_core::format::quota_fraction(exported as i64, total as i64);

    let mut els = vec![
        Element::chrome(t::PROGRESS_TITLE),
        Element::label(
            ids::MAIL_EXPORT_PROGRESS_SUMMARY,
            t::progress_summary_fmt(
                &exported.to_string(),
                &total.to_string(),
                &skipped.to_string(),
                &errored.to_string(),
            ),
        ),
        // The bar's painted text is its percentage: tui has no widget-level
        // fraction, and the shared action reads this id as text.
        Element::label(
            ids::MAIL_EXPORT_PROGRESS_BAR,
            format!("{:.0}%", fraction * 100.0),
        )
        .attr("fraction", format!("{fraction:.4}")),
        Element::gesture_button(
            ids::MAIL_EXPORT_PAUSE_BUTTON,
            t::PAUSE_BUTTON,
            true,
            Gesture::Settings(Action::MailExportPause),
        ),
        Element::gesture_button(
            ids::MAIL_EXPORT_RESUME_BUTTON,
            t::RESUME_BUTTON,
            true,
            Gesture::Settings(Action::MailExportResume),
        ),
        // Two-click inline confirm: the relabel is the gate's only visible
        // affordance, and the shared action drives it as click-then-click
        // (`actions/mail_export.py::cancel`).
        Element::gesture_button(
            ids::MAIL_EXPORT_CANCEL_BUTTON,
            if e.cancel_armed {
                fauna_i18n::strings::common::CONFIRM_Q
            } else {
                t::CANCEL_BUTTON
            },
            true,
            Gesture::Settings(Action::MailExportCancel),
        ),
        Element::label(
            ids::MAIL_EXPORT_ERROR_LOG,
            snapshot.map(|s| s.error_log.join("\n")).unwrap_or_default(),
        ),
        Element::chrome(t::ERROR_LOG_TITLE),
        Element::label(ids::MAIL_EXPORT_MAILBOX_PROGRESS_LIST, t::PROGRESS_TITLE),
    ];
    for row in snapshot
        .map(|s| s.mailbox_progress.as_slice())
        .unwrap_or(&[])
    {
        els.extend(mailbox_progress_row_elements(row));
    }
    els
}

/// One `mail-export-mailbox-progress-list-item` row. FLAT — see the module docs.
fn mailbox_progress_row_elements(
    row: &fauna_client_mail_settings::MailboxProgressView,
) -> Vec<Element> {
    vec![
        Element::label(
            ids::MAIL_EXPORT_MAILBOX_PROGRESS_LIST_ITEM,
            row.name.clone(),
        ),
        Element::label(
            ids::MAIL_EXPORT_MAILBOX_PROGRESS_LIST_ITEM_NAME,
            row.name.clone(),
        ),
        Element::label(
            ids::MAIL_EXPORT_MAILBOX_PROGRESS_LIST_ITEM_PROGRESS,
            format!("{}/{}", row.exported, row.total),
        ),
    ]
}

/// Step 5 — done. The summary card, the sealed-blob download, the copyable
/// other-device link, and the immediate discard.
///
/// The download URL is painted as text rather than opened: it is actor-bound, not
/// a public link (`mail-export.md` § Wizard steps 5), and tui's contract is to
/// surface it for copy rather than hand it to a browser.
fn done_step_elements(snapshot: Option<&MailExportSnapshot>, format: ExportFormat) -> Vec<Element> {
    let saved = snapshot
        .map(|s| s.saved_archive_path.as_str())
        .filter(|p| !p.is_empty());
    let summary = match (snapshot.and_then(|s| s.blob_bytes), saved) {
        // Once the archive is really on disk the summary says WHERE. tui has no
        // save dialog to raise (`tui.md` § Declared platform absences), so
        // without this the Download button is a control that visibly does
        // nothing — the dropped-command shape testing.md point 11 forbids — and
        // the path is exactly what the user needs next.
        (Some(bytes), Some(path)) => {
            t::saved_summary_fmt(&format_label(format), &bytes.to_string(), path)
        }
        (Some(bytes), None) => t::done_summary_fmt(&format_label(format), &bytes.to_string()),
        // Pre-completion there is no blob size yet, so the summary is the format
        // alone — linux's `done_text` shape, and why `done_summary_fmt` is not
        // simply used unconditionally with a zero.
        (None, _) => format_label(format),
    };
    vec![
        Element::chrome(t::DONE_TITLE),
        Element::label(ids::MAIL_EXPORT_DONE_SUMMARY, summary),
        Element::gesture_button(
            ids::MAIL_EXPORT_DOWNLOAD_BUTTON,
            t::DOWNLOAD_BUTTON,
            true,
            Gesture::Settings(Action::MailExportDownload),
        ),
        Element::label(
            ids::MAIL_EXPORT_DOWNLOAD_URL,
            snapshot.map(|s| s.download_url.clone()).unwrap_or_default(),
        )
        .labelled(t::DOWNLOAD_URL_LABEL),
        Element::gesture_button(
            ids::MAIL_EXPORT_DISCARD_BUTTON,
            t::DISCARD_BUTTON,
            true,
            Gesture::Settings(Action::MailExportDiscard),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SubPage;
    use fauna_client_mail_settings::{ExportStatus, MailboxOption, MailboxProgressView};

    fn a_snapshot(step: ExportStep) -> MailExportSnapshot {
        MailExportSnapshot {
            step,
            format: ExportFormat::Mbox,
            mailboxes: Vec::new(),
            date_from: String::new(),
            date_to: String::new(),
            strip_headers: false,
            session_state: None,
            exported_count: 0,
            skipped_count: 0,
            errored_count: 0,
            total_count: 0,
            mailbox_progress: Vec::new(),
            error_log: Vec::new(),
            blob_bytes: None,
            download_url: String::new(),
            saved_archive_path: String::new(),
            status: ExportStatus::Idle,
            error: None,
        }
    }

    fn state_with(snapshot: MailExportSnapshot) -> SettingsState {
        let mut state = SettingsState {
            sub: SubPage::MailExport,
            ..Default::default()
        };
        state.mail_export.snapshot = Some(snapshot);
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

    /// Every static ui.yaml id for the `mail-export` page paints — collected
    /// across the five steps, because the ratified shape is one element set shown
    /// conditionally by step (`mail-export.md:37`) and on tui "shown" means
    /// "painted". This is the tui analogue of linux's
    /// `export_page_exposes_static_ui_yaml_ids`.
    #[test]
    fn every_static_ui_yaml_id_paints_across_the_five_steps() {
        let mut painted: Vec<String> = Vec::new();
        for step in [
            ExportStep::Format,
            ExportStep::Confirm,
            ExportStep::Progress,
            ExportStep::Done,
        ] {
            painted.extend(ids(&mail_export_elements(&state_with(a_snapshot(step)))));
        }
        // The Scope step needs at least one mailbox in the snapshot for
        // `mail-export-scope-mailbox-item` (conditional on a non-empty list, the
        // `SCOPE_MAILBOXES_EMPTY` placeholder's precedent) to paint at all — the
        // `mail_import.rs` precedent for this same test shape.
        let mut scope_snap = a_snapshot(ExportStep::Scope);
        scope_snap.mailboxes = vec![MailboxOption {
            name: "INBOX".into(),
            selected: true,
        }];
        painted.extend(ids(&mail_export_elements(&state_with(scope_snap))));
        for id in [
            "page-heading",
            "mail-export-format-picker",
            "mail-export-scope-mailboxes",
            "mail-export-scope-mailbox-item",
            "mail-export-scope-date-from",
            "mail-export-scope-date-to",
            "mail-export-scope-strip-headers-toggle",
            "wizard-next-button",
            "wizard-back-button",
            "mail-export-confirm-summary",
            "mail-export-start-button",
            "mail-export-progress-summary",
            "mail-export-progress-bar",
            "mail-export-pause-button",
            "mail-export-resume-button",
            "mail-export-cancel-button",
            "mail-export-error-log",
            "mail-export-done-summary",
            "mail-export-download-button",
            "mail-export-download-url",
            "mail-export-discard-button",
            "mail-export-mailbox-progress-list",
            "settings-nav-back",
        ] {
            assert!(
                painted.contains(&id.to_string()),
                "missing {id:?}; painted {painted:?}"
            );
        }
    }

    /// Each step paints only its OWN ids — the conditional-by-step contract. A
    /// page that painted all five at once would let the driver click Start while
    /// the user is still on the format picker.
    #[test]
    fn each_step_paints_only_its_own_ids() {
        let format = ids(&mail_export_elements(&state_with(a_snapshot(
            ExportStep::Format,
        ))));
        assert!(format.iter().any(|id| id == "mail-export-format-picker"));
        assert!(!format.iter().any(|id| id == "mail-export-start-button"));
        assert!(!format.iter().any(|id| id == "mail-export-scope-date-from"));

        let scope = ids(&mail_export_elements(&state_with(a_snapshot(
            ExportStep::Scope,
        ))));
        assert!(scope.iter().any(|id| id == "mail-export-scope-date-from"));
        assert!(!scope.iter().any(|id| id == "mail-export-format-picker"));

        let done = ids(&mail_export_elements(&state_with(a_snapshot(
            ExportStep::Done,
        ))));
        assert!(done.iter().any(|id| id == "mail-export-download-button"));
        assert!(!done.iter().any(|id| id == "mail-export-pause-button"));
    }

    /// Pre-hydrate — the whole point of this page against an unbuilt backend —
    /// the wizard still opens on step 1 with a usable picker, and does NOT claim
    /// an empty mailbox list it has never read.
    #[test]
    fn pre_hydrate_the_wizard_still_opens_on_a_usable_format_step() {
        let state = SettingsState {
            sub: SubPage::MailExport,
            ..Default::default()
        };
        let els = mail_export_elements(&state);
        assert!(ids(&els).contains(&"mail-export-format-picker".to_string()));
        assert_eq!(
            text_of(&els, "mail-export-format-picker", 0),
            format_label(ExportFormat::Mbox),
            "the picker opens on the ratified default format"
        );
        assert!(
            !els.iter().any(|e| e.text == t::SCOPE_MAILBOXES_EMPTY),
            "an unread mailbox list must not claim to be empty"
        );
    }

    /// The picker's options and its painted value both come from the SHARED
    /// `export_format_label` map, so a seventh app cannot drift from the six
    /// before it (`mail-export.md:15`).
    #[test]
    fn the_format_picker_round_trips_the_shared_label() {
        let mut snap = a_snapshot(ExportStep::Format);
        snap.format = ExportFormat::EmlZip;
        let els = mail_export_elements(&state_with(snap));
        assert_eq!(
            text_of(&els, "mail-export-format-picker", 0),
            crate::wizard::localized(&export_format_label(ExportFormat::EmlZip)),
        );
        let options = match &els
            .iter()
            .find(|e| e.id == "mail-export-format-picker")
            .expect("picker")
            .role
        {
            crate::element::Role::Select { options, .. } => options.clone(),
            other => panic!("the format picker must be a real Select, got {other:?}"),
        };
        assert_eq!(
            options,
            vec![
                crate::wizard::localized(&export_format_label(ExportFormat::Mbox)),
                crate::wizard::localized(&export_format_label(ExportFormat::MaildirPlus)),
                crate::wizard::localized(&export_format_label(ExportFormat::EmlZip)),
            ]
        );
        // …and every offered label resolves back to the format it names, which is
        // what makes the shared `select(id, label)` call land on the right arm.
        for f in [
            ExportFormat::Mbox,
            ExportFormat::MaildirPlus,
            ExportFormat::EmlZip,
        ] {
            assert_eq!(format_for_label(&format_label(f)), f);
        }
    }

    /// The progress bar reads the SHARED `quota_fraction` (`mail-export.md:17`),
    /// zero-guarded and clamped — not a local `exported / total`.
    #[test]
    fn the_progress_bar_uses_the_shared_quota_fraction() {
        let mut snap = a_snapshot(ExportStep::Progress);
        snap.exported_count = 3;
        snap.total_count = 4;
        let els = mail_export_elements(&state_with(snap));
        assert_eq!(text_of(&els, "mail-export-progress-bar", 0), "75%");

        // The zero-total guard: a bar that divided locally would be NaN here.
        let els = mail_export_elements(&state_with(a_snapshot(ExportStep::Progress)));
        assert_eq!(text_of(&els, "mail-export-progress-bar", 0), "0%");
    }

    /// All three summary lines render through the GENERATED i18n templates, not
    /// hard-coded English — the drift this slice closed. linux/apple/windows each
    /// interpolated these byte-identical strings inline, so they were both
    /// duplicated four ways and untranslatable; `mail_export.*_summary_fmt` is now
    /// the one source of truth, with NAMED placeholders per the i18n rule.
    ///
    /// Two distinct things are pinned, deliberately: the **rendered text** is the
    /// pre-existing 4-app form verbatim (so the swap is provably
    /// zero-behavior-change, and an `en.yaml` edit that silently changed a
    /// shipped string goes red), and each **key resolves in the i18n registry**
    /// (so ripping the keys back out in favour of hard-coded English goes red
    /// too). Comparing only against the generated helper would be tautological —
    /// both sides would move together — which is why the literal strings are here.
    #[test]
    fn all_three_summary_lines_render_through_the_i18n_templates() {
        // The keys exist in the registry — what makes these lines translatable at
        // all, and the half a same-text inline `format!` would silently lose.
        for key in [
            "mail_export.confirm_summary_fmt",
            "mail_export.progress_summary_fmt",
            "mail_export.done_summary_fmt",
        ] {
            assert!(
                fauna_i18n::strings::lookup(key).is_some(),
                "{key} must resolve — a summary line rendered off a missing key is \
                 hard-coded English wearing an i18n costume"
            );
        }

        let mut snap = a_snapshot(ExportStep::Progress);
        snap.exported_count = 3;
        snap.total_count = 10;
        snap.skipped_count = 1;
        snap.errored_count = 2;
        let els = mail_export_elements(&state_with(snap));
        assert_eq!(
            text_of(&els, "mail-export-progress-summary", 0),
            "3 of 10 · 1 skipped · 2 errored"
        );
        assert_eq!(
            text_of(&els, "mail-export-progress-summary", 0),
            t::progress_summary_fmt("3", "10", "1", "2"),
            "the painted line must come from the generated template, not a local format!"
        );

        let mut snap = a_snapshot(ExportStep::Confirm);
        snap.mailboxes = vec![MailboxOption {
            name: "INBOX".into(),
            selected: true,
        }];
        let els = mail_export_elements(&state_with(snap));
        assert_eq!(
            text_of(&els, "mail-export-confirm-summary", 0),
            t::confirm_summary_fmt(&format_label(ExportFormat::Mbox), "1"),
        );

        let mut snap = a_snapshot(ExportStep::Done);
        snap.blob_bytes = Some(4096);
        let els = mail_export_elements(&state_with(snap));
        assert_eq!(
            text_of(&els, "mail-export-done-summary", 0),
            t::done_summary_fmt(&format_label(ExportFormat::Mbox), "4096"),
        );
    }

    /// One row per mailbox with every leaf present, painted FLAT — which is what
    /// makes the shared action's plain `count(...)` resolve (see the module docs).
    #[test]
    fn mailbox_progress_rows_paint_flat_with_every_leaf() {
        let mut snap = a_snapshot(ExportStep::Progress);
        snap.mailbox_progress = vec![
            MailboxProgressView {
                name: "INBOX".into(),
                exported: 10,
                total: 20,
            },
            MailboxProgressView {
                name: "Sent".into(),
                exported: 5,
                total: 5,
            },
        ];
        let els = mail_export_elements(&state_with(snap));
        for id in [
            "mail-export-mailbox-progress-list-item",
            "mail-export-mailbox-progress-list-item-name",
            "mail-export-mailbox-progress-list-item-progress",
        ] {
            assert_eq!(
                els.iter().filter(|e| e.id == id).count(),
                2,
                "one {id} per mailbox"
            );
        }
        assert_eq!(
            text_of(&els, "mail-export-mailbox-progress-list-item-name", 0),
            "INBOX"
        );
        assert_eq!(
            text_of(&els, "mail-export-mailbox-progress-list-item-progress", 1),
            "5/5"
        );
        assert!(
            els.iter().all(|e| e.path.is_empty()),
            "every leaf on this page paints flat — a container path would break \
             the shared action's plain index reads"
        );
    }

    /// The scope step's mailbox rows reflect the snapshot's selection, and the
    /// container element paints even when the list is empty (the driver counts it).
    #[test]
    fn the_scope_step_paints_the_mailbox_selection() {
        let mut snap = a_snapshot(ExportStep::Scope);
        snap.mailboxes = vec![
            MailboxOption {
                name: "INBOX".into(),
                selected: true,
            },
            MailboxOption {
                name: "Trash".into(),
                selected: false,
            },
        ];
        let els = mail_export_elements(&state_with(snap));
        assert!(ids(&els).contains(&"mail-export-scope-mailboxes".to_string()));
        let states: Vec<Option<String>> = els
            .iter()
            .filter(|e| e.id == "mail-export-scope-mailbox-item")
            .map(|e| {
                e.attrs
                    .iter()
                    .find(|(k, _)| k == "state")
                    .map(|(_, v)| v.clone())
            })
            .collect();
        assert_eq!(
            states,
            vec![Some("on".to_string()), Some("off".to_string())],
            "the ratified default deselects Trash and selects INBOX"
        );
        assert!(
            !els.iter().any(|e| e.text == t::SCOPE_MAILBOXES_EMPTY),
            "a populated list must not paint the empty placeholder"
        );
    }

    /// The strip-headers toggle carries the `state` attr the shared action reads,
    /// and it reflects the SNAPSHOT — so it can only be flipped by a dispatch that
    /// actually reached the machine, never by a local click echo.
    #[test]
    fn the_strip_headers_toggle_carries_a_snapshot_backed_state_attr() {
        let attr = |strip: bool| {
            let mut snap = a_snapshot(ExportStep::Scope);
            snap.strip_headers = strip;
            mail_export_elements(&state_with(snap))
                .into_iter()
                .find(|e| e.id == "mail-export-scope-strip-headers-toggle")
                .and_then(|e| {
                    e.attrs
                        .iter()
                        .find(|(k, _)| k == "state")
                        .map(|(_, v)| v.clone())
                })
        };
        assert_eq!(attr(false).as_deref(), Some("off"));
        assert_eq!(attr(true).as_deref(), Some("on"));
    }

    /// The cancel button relabels once armed — the two-click gate's only visible
    /// affordance, and the reason one stray click cannot abort a running export
    /// and unlink its partial blob.
    #[test]
    fn the_cancel_button_relabels_when_armed() {
        let mut state = state_with(a_snapshot(ExportStep::Progress));
        let unarmed = text_of(
            &mail_export_elements(&state),
            "mail-export-cancel-button",
            0,
        );
        state.mail_export.cancel_armed = true;
        let armed = text_of(
            &mail_export_elements(&state),
            "mail-export-cancel-button",
            0,
        );
        assert_ne!(unarmed, armed, "the armed cancel must be visibly distinct");
        assert_eq!(armed, fauna_i18n::strings::common::CONFIRM_Q);
    }

    /// A fresh visit disarms a primed cancel and drops the date drafts, so neither
    /// survives a nav-away to act against a later visit.
    #[test]
    fn a_fresh_visit_disarms_cancel_and_drops_the_date_drafts() {
        let mut s = MailExportState {
            cancel_armed: true,
            date_from_input: "2026-01-01".into(),
            date_to_input: "2026-02-01".into(),
            ..MailExportState::default()
        };
        s.reset_form();
        assert!(!s.cancel_armed);
        assert!(s.date_from_input.is_empty());
        assert!(s.date_to_input.is_empty());
    }

    /// Advancing off the Scope step commits BOTH date drafts before it advances —
    /// in that order, in one awaited op. Were the commits dropped (or fired after
    /// `Next`), the Confirm summary would describe a scope the machine never got.
    #[test]
    fn advancing_off_scope_commits_both_dates_before_next() {
        let s = MailExportState {
            date_from_input: "  2026-01-01  ".into(),
            date_to_input: "2026-02-01".into(),
            ..MailExportState::default()
        };
        assert_eq!(
            s.scope_next_actions(),
            vec![
                MailExportAction::SetDateFrom {
                    value: "2026-01-01".into()
                },
                MailExportAction::SetDateTo {
                    value: "2026-02-01".into()
                },
                MailExportAction::Next,
            ],
            "both dates commit, trimmed, and only then does the wizard advance"
        );
    }

    /// The confirm summary counts only SELECTED mailboxes, through the shared
    /// format label — the number the user is about to commit to.
    #[test]
    fn the_confirm_summary_counts_only_selected_mailboxes() {
        let mut snap = a_snapshot(ExportStep::Confirm);
        snap.format = ExportFormat::MaildirPlus;
        snap.mailboxes = vec![
            MailboxOption {
                name: "INBOX".into(),
                selected: true,
            },
            MailboxOption {
                name: "Sent".into(),
                selected: true,
            },
            MailboxOption {
                name: "Trash".into(),
                selected: false,
            },
        ];
        let els = mail_export_elements(&state_with(snap));
        assert_eq!(
            text_of(&els, "mail-export-confirm-summary", 0),
            format!(
                "{} · 2 mailbox(es)",
                format_label(ExportFormat::MaildirPlus)
            ),
        );
    }

    /// The done step paints the blob size when the session reports one, and the
    /// actor-bound download URL as copyable text.
    #[test]
    fn the_done_step_paints_the_blob_size_and_download_url() {
        let mut snap = a_snapshot(ExportStep::Done);
        snap.blob_bytes = Some(4096);
        snap.download_url = "/api/v1/export/abc".into();
        let els = mail_export_elements(&state_with(snap));
        assert_eq!(
            text_of(&els, "mail-export-done-summary", 0),
            format!("{} · 4096 bytes", format_label(ExportFormat::Mbox)),
        );
        assert_eq!(
            text_of(&els, "mail-export-download-url", 0),
            "/api/v1/export/abc"
        );
    }

    /// Pressing Download must be visible. tui raises no save dialog
    /// (`tui.md` § Declared platform absences), so once the archive is on disk
    /// the Done summary is the only surface that can say so — and a control
    /// whose effect is invisible is the dropped-command shape testing.md point
    /// 11 forbids. The download URL stays the URL: it is the other-device link,
    /// not this device's file.
    #[test]
    fn the_done_summary_names_the_saved_archive_once_it_is_on_disk() {
        let mut done = a_snapshot(ExportStep::Done);
        done.blob_bytes = Some(4096);
        done.download_url = "/api/v1/export/abc".into();
        done.saved_archive_path =
            "/home/user/Downloads/fauna-export-alice-mbox-2026-09-22.zip.zst".into();
        let els = mail_export_elements(&state_with(done));
        assert_eq!(
            text_of(&els, "mail-export-done-summary", 0),
            t::saved_summary_fmt(
                &format_label(ExportFormat::Mbox),
                "4096",
                "/home/user/Downloads/fauna-export-alice-mbox-2026-09-22.zip.zst",
            ),
        );
        assert_eq!(
            text_of(&els, "mail-export-download-url", 0),
            "/api/v1/export/abc"
        );
    }
}
