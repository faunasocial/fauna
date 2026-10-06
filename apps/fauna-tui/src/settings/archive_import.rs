//! The Settings → Import from other services wizard (`docs/goal/behavior/
//! archive-import.md` § The wizard and its machine; `tests/e2e-unified/ui.yaml`
//! `archive-import` page + its `archive-import-category-progress-list`
//! component, user-approved 2026-09-08).
//!
//! A six-screen wizard (Source → Archive → Scope → Confirm → Progress → Done)
//! that is a dumb renderer of the shared
//! `fauna_archive_import_machine::ArchiveImportSnapshot` and a dispatcher of
//! `ArchiveImportAction`. The FSM, the parser, the folder writes and the
//! authoring are shared Rust (priority #2); tui is the lead app (2026-09-08),
//! no other app renders this page yet.
//!
//! # The archive is a PATH here
//!
//! tui has no file picker (`tui.md` § Declared platform absences item 4): the
//! Archive step is a path field plus `archive-import-archive-open-button`,
//! which commits the path buffer and dispatches `OpenArchive` — one awaited
//! op, the `mail_import.rs` buffer-commit shape.
//!
//! # Three buttons and what they honestly do
//!
//! `archive-import-view-imported-button` → `Gesture::Nav(Page::Feed)` (the
//! feed carries no per-source filter yet). `archive-import-review-skipped-button`
//! toggles the skip log onto the Done screen (`ArchiveImportState::show_skipped`).
//! `archive-import-profile-prefill-button` paints DISABLED: the machine exposes
//! no archive profile yet, so there is nothing to prefill — offered, never
//! applied silently, and never faked (testing.md point 11; the slice-4 gap
//! named in `archive-import.md` § Implementation status today).

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_archive_import_machine::{
    ArchiveImportAction, ArchiveImportMachine, ArchiveImportSnapshot, ArchiveImportStep,
    ArchiveSourceKind, AudienceMode, RunState,
};
use fauna_i18n::strings::archive_import as t;

use super::{Action, SettingsField, SettingsState};
use crate::element::{Element, Field, Gesture, SelectTarget};
use crate::pages::Page;

/// The rail row's label and the page heading.
pub(crate) const TITLE: &str = t::TITLE;

/// The Import-from-other-services sub-page's state, the `MailImportState` shape.
///
/// `Default` is derived rather than hand-written (the one shape difference from
/// `MailImportState`, whose `max_size_input` seeds a non-empty preset): every
/// field here starts at its own type's default, and a hand-written impl saying
/// exactly that is what `clippy::derivable_impls` refuses.
#[derive(Default)]
pub(crate) struct ArchiveImportState {
    /// The shared wizard machine. `None` pre-auth, or when the glue refused
    /// (`archive_glue::build_archive_import_machine`'s `Err`, kept for the page).
    pub(super) machine: Option<Arc<ArchiveImportMachine>>,
    pub(super) unavailable: Option<String>,
    /// The last rendered snapshot. `None` until the first hydrate resolves.
    pub(super) snapshot: Option<ArchiveImportSnapshot>,
    /// Step 2 — local draft, committed by `open_actions` on Open archive.
    pub(super) path_input: String,
    /// Step 3 — local drafts, committed by `scope_next_actions` on Next.
    pub(super) date_from_input: String,
    pub(super) date_to_input: String,
    /// Set once Cancel has been armed by a first click.
    pub(super) cancel_armed: bool,
    /// Step 6 — the skip log is shown on the Done screen.
    pub(super) show_skipped: bool,
}

impl ArchiveImportState {
    /// Build the page's shared machine from the session's WS handle and actor
    /// at the post-auth hook. A refusal is kept, not swallowed: the page paints
    /// it.
    pub(super) fn build(
        nest: Arc<fauna_client::NestClient>,
        actor_id_hex: &str,
        period_keys: fauna_client_subscriptions::SharedPeriodKeyStore,
        folder_keys: std::sync::Arc<dyn fauna_client_folders::FolderKeyStore>,
        mail: Arc<dyn fauna_client_config::MailStore>,
    ) -> Self {
        match crate::archive_glue::build_archive_import_machine(
            nest,
            actor_id_hex,
            period_keys,
            folder_keys,
            mail,
        ) {
            Ok(machine) => Self {
                machine: Some(Arc::new(machine)),
                ..Self::default()
            },
            Err(reason) => {
                tracing::warn!("[settings/archive_import] build machine: {reason}");
                Self {
                    unavailable: Some(reason),
                    ..Self::default()
                }
            }
        }
    }

    /// Drop the page-local drafts on a fresh visit (the `MailImportState`
    /// shape). The snapshot itself is re-hydrated by the visit.
    pub(super) fn reset_form(&mut self) {
        self.cancel_armed = false;
        self.show_skipped = false;
        self.path_input.clear();
        self.date_from_input.clear();
        self.date_to_input.clear();
    }

    /// Step 2: commit the path draft, then open — one awaited op.
    pub(super) fn open_actions(&self) -> Vec<ArchiveImportAction> {
        vec![
            ArchiveImportAction::SetArchivePath {
                value: self.path_input.trim().to_string(),
            },
            ArchiveImportAction::OpenArchive,
        ]
    }

    /// Step 3→4: commit both date drafts (trimmed), then advance.
    pub(super) fn scope_next_actions(&self) -> Vec<ArchiveImportAction> {
        vec![
            ArchiveImportAction::SetDateFrom {
                value: self.date_from_input.trim().to_string(),
            },
            ArchiveImportAction::SetDateTo {
                value: self.date_to_input.trim().to_string(),
            },
            ArchiveImportAction::Next,
        ]
    }
}

/// The picker's option text — the crate's canonical platform label (a brand
/// name, not translated), a **label** round-trip like `MailImportSourceKind`.
pub(crate) fn source_kind_label(kind: ArchiveSourceKind) -> String {
    kind.label().to_string()
}

pub(crate) const SOURCE_KINDS: [ArchiveSourceKind; 2] =
    [ArchiveSourceKind::Facebook, ArchiveSourceKind::Instagram];

/// An unrecognized label keeps the default (Facebook) rather than panicking —
/// the `mail_import::source_kind_for_label` contract.
pub(crate) fn source_kind_for_label(label: &str) -> ArchiveSourceKind {
    SOURCE_KINDS
        .into_iter()
        .find(|k| source_kind_label(*k) == label)
        .unwrap_or(ArchiveSourceKind::Facebook)
}

pub(crate) const AUDIENCE_MODES: [AudienceMode; 2] = [AudienceMode::Original, AudienceMode::OnlyMe];

pub(crate) fn audience_mode_label(mode: AudienceMode) -> String {
    match mode {
        AudienceMode::Original => t::AUDIENCE_ORIGINAL.to_string(),
        AudienceMode::OnlyMe => t::AUDIENCE_ONLY_ME.to_string(),
    }
}

pub(crate) fn audience_mode_for_label(label: &str) -> AudienceMode {
    AUDIENCE_MODES
        .into_iter()
        .find(|m| audience_mode_label(*m) == label)
        .unwrap_or(AudienceMode::Original)
}

/// The sub-page's ordered element list — only the ACTIVE step's ids, the
/// `mail_import_elements` shape. `error-message` is registered globally by
/// `crate::ui::register_frame`.
pub(super) fn archive_import_elements(state: &SettingsState) -> Vec<Element> {
    let e = &state.archive_import;
    let mut els = vec![Element::label(ids::PAGE_HEADING, t::TITLE)];
    if let Some(_reason) = &e.unavailable {
        // The reason itself is logged at build time; the page states the
        // user-facing fact and offers nothing it could not commit.
        els.push(Element::chrome(t::UNAVAILABLE));
        els.push(nav_back());
        return els;
    }
    els.push(Element::chrome(t::DESCRIPTION));
    let snapshot = e.snapshot.as_ref();
    // Pre-hydrate the wizard opens on step 1 with the default platform: the
    // client-side steps do not depend on the nest.
    let step = snapshot
        .map(|s| s.step)
        .unwrap_or(ArchiveImportStep::Source);
    match step {
        ArchiveImportStep::Source => els.extend(source_step_elements(snapshot)),
        ArchiveImportStep::Archive => els.extend(archive_step_elements(e, snapshot)),
        ArchiveImportStep::Scope => els.extend(scope_step_elements(e, snapshot)),
        ArchiveImportStep::Confirm => els.extend(confirm_step_elements(snapshot)),
        ArchiveImportStep::Progress => els.extend(progress_step_elements(e, snapshot)),
        ArchiveImportStep::Done => els.extend(done_step_elements(e, snapshot)),
    }
    els.push(nav_back());
    els
}

fn nav_back() -> Element {
    Element::gesture_button(
        ids::SETTINGS_NAV_BACK,
        fauna_i18n::strings::common::BACK,
        true,
        Gesture::Settings(Action::NavBack),
    )
    .nav_back()
}

fn next_button() -> Element {
    Element::gesture_button(
        ids::WIZARD_NEXT_BUTTON,
        t::NEXT,
        true,
        Gesture::Settings(Action::ArchiveImportNext),
    )
}

fn back_button() -> Element {
    Element::gesture_button(
        ids::WIZARD_BACK_BUTTON,
        t::BACK,
        true,
        Gesture::Settings(Action::ArchiveImportBack),
    )
}

/// `YYYY-MM-DD` (UTC) of an epoch-micros instant — the workspace's one civil
/// calendar (`fauna_core::caltime`, priority #2), the `run.rs::today_ymd` shape.
fn ymd(micros: u64) -> String {
    let days = (micros / 1_000_000 / 86_400) as i64;
    let (year, month, day) = fauna_core::caltime::civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

fn bytes_text(bytes: u64) -> String {
    crate::wizard::localized(&fauna_core::format::byte_size(bytes))
}

/// The localized label of a `fauna_archive::Category` token; an unknown token
/// (a newer machine) shows itself rather than nothing.
fn category_label(token: &str) -> String {
    match token {
        "posts" => t::CATEGORY_POSTS,
        "albums" => t::CATEGORY_ALBUMS,
        "comments" => t::CATEGORY_COMMENTS,
        "reactions" => t::CATEGORY_REACTIONS,
        "events" => t::CATEGORY_EVENTS,
        "groups" => t::CATEGORY_GROUPS,
        "friends" => t::CATEGORY_FRIENDS,
        "threads" => t::CATEGORY_THREADS,
        "messages" => t::CATEGORY_MESSAGES,
        "profile" => t::CATEGORY_PROFILE,
        other => return other.to_string(),
    }
    .to_string()
}

fn run_state_label(state: Option<RunState>) -> &'static str {
    match state {
        Some(RunState::Running) | None => t::STATE_RUNNING,
        Some(RunState::Paused) => t::STATE_PAUSED,
        Some(RunState::Cancelled) => t::STATE_CANCELLED,
        Some(RunState::Completed) => t::STATE_COMPLETED,
        Some(RunState::Errored) => t::STATE_ERRORED,
    }
}

/// Step 1 — the platform picker and how to request the export there.
fn source_step_elements(snapshot: Option<&ArchiveImportSnapshot>) -> Vec<Element> {
    let kind = snapshot
        .map(|s| s.source_kind)
        .unwrap_or(ArchiveSourceKind::Facebook);
    vec![
        Element::chrome(t::SOURCE_TITLE),
        Element::select(
            ids::ARCHIVE_IMPORT_SOURCE_PICKER,
            source_kind_label(kind),
            SelectTarget::ArchiveImportSourceKind,
            SOURCE_KINDS.into_iter().map(source_kind_label).collect(),
        )
        .labelled(t::SOURCE_TITLE),
        Element::label(
            ids::ARCHIVE_IMPORT_SOURCE_HELP,
            match kind {
                ArchiveSourceKind::Facebook => t::SOURCE_HELP_FACEBOOK,
                ArchiveSourceKind::Instagram => t::SOURCE_HELP_INSTAGRAM,
            },
        ),
        next_button(),
    ]
}

/// Step 2 — the path field, the open button, and the summary once indexed.
fn archive_step_elements(
    e: &ArchiveImportState,
    snapshot: Option<&ArchiveImportSnapshot>,
) -> Vec<Element> {
    let mut els = vec![
        Element::chrome(t::ARCHIVE_TITLE),
        Element::input(
            ids::ARCHIVE_IMPORT_ARCHIVE_PATH,
            e.path_input.clone(),
            Field::Settings(SettingsField::ArchiveImportPath),
        )
        .labelled(t::ARCHIVE_PATH_PLACEHOLDER),
        Element::gesture_button(
            ids::ARCHIVE_IMPORT_ARCHIVE_OPEN_BUTTON,
            t::ARCHIVE_OPEN_BUTTON,
            true,
            Gesture::Settings(Action::ArchiveImportOpenArchive),
        ),
    ];
    if let Some(summary) = snapshot.and_then(|s| s.summary.as_ref()) {
        let archive_size = bytes_text(summary.archive_bytes);
        let media_size = bytes_text(summary.media_bytes);
        let text = if summary.first_at == 0 {
            t::archive_summary_undated(
                &summary.platform_label,
                &summary.owner_display_name,
                &archive_size,
                &media_size,
            )
        } else {
            t::archive_summary_fmt(
                &summary.platform_label,
                &summary.owner_display_name,
                &ymd(summary.first_at),
                &ymd(summary.last_at),
                &archive_size,
                &media_size,
            )
        };
        els.push(Element::label(ids::ARCHIVE_IMPORT_ARCHIVE_SUMMARY, text));
    }
    els.push(back_button());
    els.push(next_button());
    els
}

/// Step 3 — the category rows, the audience mode + its sentence, the dates.
fn scope_step_elements(
    e: &ArchiveImportState,
    snapshot: Option<&ArchiveImportSnapshot>,
) -> Vec<Element> {
    let mut els = vec![
        Element::chrome(t::SCOPE_TITLE),
        Element::label(
            ids::ARCHIVE_IMPORT_SCOPE_CATEGORIES,
            t::SCOPE_CATEGORIES_LABEL,
        ),
    ];
    for row in snapshot.map(|s| s.categories.as_slice()).unwrap_or(&[]) {
        let label = category_label(&row.token);
        let count = row.count.to_string();
        if row.importable {
            els.push(
                Element::checkbox_gesture(
                    ids::ARCHIVE_IMPORT_SCOPE_CATEGORY_ITEM,
                    t::scope_category_row_fmt(&label, &count),
                    row.selected,
                    Gesture::Settings(Action::ArchiveImportToggleCategory(row.token.clone())),
                )
                .attr("state", if row.selected { "on" } else { "off" }),
            );
        } else {
            // Model-only in phase one: kept in the folder, never selectable
            // (§ What each category becomes) — a label, not a checkbox.
            els.push(
                Element::label(
                    ids::ARCHIVE_IMPORT_SCOPE_CATEGORY_ITEM,
                    t::scope_category_kept_fmt(&label, &count),
                )
                .attr("state", "kept"),
            );
        }
    }
    let mode = snapshot
        .map(|s| s.audience_mode)
        .unwrap_or(AudienceMode::Original);
    els.push(
        Element::select(
            ids::ARCHIVE_IMPORT_SCOPE_AUDIENCE_MODE,
            audience_mode_label(mode),
            SelectTarget::ArchiveImportAudienceMode,
            AUDIENCE_MODES
                .into_iter()
                .map(audience_mode_label)
                .collect(),
        )
        .labelled(t::SCOPE_AUDIENCE_MODE_LABEL),
    );
    let (known, unknown) = snapshot
        .and_then(|s| s.summary.as_ref())
        .map(|s| (s.known_audience, s.unknown_audience))
        .unwrap_or((0, 0));
    els.push(Element::label(
        ids::ARCHIVE_IMPORT_SCOPE_AUDIENCE_SUMMARY,
        match mode {
            AudienceMode::OnlyMe => t::SCOPE_AUDIENCE_SUMMARY_ONLY_ME.to_string(),
            AudienceMode::Original => {
                t::scope_audience_summary_fmt(&known.to_string(), &unknown.to_string())
            }
        },
    ));
    if snapshot.is_some_and(|s| s.nest_supports_hidden_tiers == Some(false)) {
        els.push(Element::chrome(t::SCOPE_HIDDEN_TIERS_UNAVAILABLE));
    }
    els.push(
        Element::input(
            ids::ARCHIVE_IMPORT_SCOPE_DATE_FROM,
            e.date_from_input.clone(),
            Field::Settings(SettingsField::ArchiveImportDateFrom),
        )
        .labelled(t::SCOPE_DATE_FROM_PLACEHOLDER),
    );
    els.push(
        Element::input(
            ids::ARCHIVE_IMPORT_SCOPE_DATE_TO,
            e.date_to_input.clone(),
            Field::Settings(SettingsField::ArchiveImportDateTo),
        )
        .labelled(t::SCOPE_DATE_TO_PLACEHOLDER),
    );
    els.push(back_button());
    els.push(next_button());
    els
}

/// Step 4 — totals and the durable commit.
fn confirm_step_elements(snapshot: Option<&ArchiveImportSnapshot>) -> Vec<Element> {
    let (records, bytes) = snapshot
        .map(|s| (s.confirm_records, s.confirm_bytes))
        .unwrap_or((0, 0));
    vec![
        Element::chrome(t::CONFIRM_TITLE),
        Element::label(
            ids::ARCHIVE_IMPORT_CONFIRM_SUMMARY,
            t::confirm_summary_fmt(&records.to_string(), &bytes_text(bytes)),
        ),
        back_button(),
        Element::gesture_button(
            ids::ARCHIVE_IMPORT_START_BUTTON,
            t::START_BUTTON,
            true,
            Gesture::Settings(Action::ArchiveImportStart),
        ),
    ]
}

/// Step 5 — counters, the bar, pause/resume/cancel, the skip log, the
/// per-category rows (FLAT — the `mail_import.rs` progress-row shape).
fn progress_step_elements(
    e: &ArchiveImportState,
    snapshot: Option<&ArchiveImportSnapshot>,
) -> Vec<Element> {
    let (imported, skipped, total, run_state) = snapshot
        .map(|s| (s.imported, s.skipped, s.total, s.run_state))
        .unwrap_or((0, 0, 0, None));
    let fraction = fauna_core::format::quota_fraction((imported + skipped) as i64, total as i64);
    let mut els = vec![
        Element::chrome(t::PROGRESS_TITLE),
        Element::label(
            ids::ARCHIVE_IMPORT_PROGRESS_SUMMARY,
            t::progress_summary_fmt(
                run_state_label(run_state),
                &imported.to_string(),
                &total.to_string(),
                &skipped.to_string(),
            ),
        ),
        Element::label(
            ids::ARCHIVE_IMPORT_PROGRESS_BAR,
            format!("{:.0}%", fraction * 100.0),
        )
        .attr("fraction", format!("{fraction:.4}")),
        Element::gesture_button(
            ids::ARCHIVE_IMPORT_PAUSE_BUTTON,
            t::PAUSE_BUTTON,
            true,
            Gesture::Settings(Action::ArchiveImportPause),
        ),
        Element::gesture_button(
            ids::ARCHIVE_IMPORT_RESUME_BUTTON,
            t::RESUME_BUTTON,
            true,
            Gesture::Settings(Action::ArchiveImportResume),
        ),
        Element::gesture_button(
            ids::ARCHIVE_IMPORT_CANCEL_BUTTON,
            if e.cancel_armed {
                fauna_i18n::strings::common::CONFIRM_Q
            } else {
                t::CANCEL_BUTTON
            },
            true,
            Gesture::Settings(Action::ArchiveImportCancel),
        ),
        Element::label(
            ids::ARCHIVE_IMPORT_ERROR_LOG,
            snapshot.map(|s| s.skip_log.join("\n")).unwrap_or_default(),
        ),
        Element::chrome(t::ERROR_LOG_TITLE),
        Element::label(
            ids::ARCHIVE_IMPORT_CATEGORY_PROGRESS_LIST,
            t::PROGRESS_TITLE,
        ),
    ];
    for row in snapshot.map(|s| s.categories.as_slice()).unwrap_or(&[]) {
        if !(row.importable && row.selected) {
            continue;
        }
        let label = category_label(&row.token);
        els.push(Element::label(
            ids::ARCHIVE_IMPORT_CATEGORY_PROGRESS_LIST_ITEM,
            label.clone(),
        ));
        els.push(Element::label(
            ids::ARCHIVE_IMPORT_CATEGORY_PROGRESS_LIST_ITEM_NAME,
            label,
        ));
        els.push(Element::label(
            ids::ARCHIVE_IMPORT_CATEGORY_PROGRESS_LIST_ITEM_PROGRESS,
            t::progress_row_fmt(
                &row.imported.to_string(),
                &row.count.to_string(),
                &row.skipped.to_string(),
            ),
        ));
    }
    // A concluded run may be left (the machine starts a fresh wizard on Back);
    // a running, paused or errored one is resumable and has no Back.
    if matches!(run_state, Some(RunState::Cancelled | RunState::Completed)) {
        els.push(back_button());
    }
    els
}

/// Step 6 — counts, the feed link, the skip log behind a toggle, the (inert)
/// profile offer, the folder link, and Back into a fresh wizard.
fn done_step_elements(
    e: &ArchiveImportState,
    snapshot: Option<&ArchiveImportSnapshot>,
) -> Vec<Element> {
    let (imported, skipped) = snapshot.map(|s| (s.imported, s.skipped)).unwrap_or((0, 0));
    let mut els = vec![
        Element::chrome(t::DONE_TITLE),
        Element::label(
            ids::ARCHIVE_IMPORT_DONE_SUMMARY,
            t::done_summary_fmt(&imported.to_string(), &skipped.to_string()),
        ),
        Element::gesture_button(
            ids::ARCHIVE_IMPORT_VIEW_IMPORTED_BUTTON,
            t::VIEW_IMPORTED_BUTTON,
            true,
            Gesture::Nav(Page::Feed),
        ),
        Element::gesture_button(
            ids::ARCHIVE_IMPORT_REVIEW_SKIPPED_BUTTON,
            t::REVIEW_SKIPPED_BUTTON,
            true,
            Gesture::Settings(Action::ArchiveImportToggleSkipped),
        ),
    ];
    if e.show_skipped {
        els.push(Element::label(
            ids::ARCHIVE_IMPORT_ERROR_LOG,
            snapshot.map(|s| s.skip_log.join("\n")).unwrap_or_default(),
        ));
    }
    // Offered, never applied silently — and DISABLED: the machine exposes no
    // archive profile yet (module docs).
    els.push(Element::gesture_button(
        ids::ARCHIVE_IMPORT_PROFILE_PREFILL_BUTTON,
        t::PROFILE_PREFILL_BUTTON,
        false,
        Gesture::Settings(Action::ArchiveImportProfilePrefill),
    ));
    let folder = snapshot
        .and_then(|s| s.folder_name.clone())
        .unwrap_or_default();
    els.push(Element::gesture_button(
        ids::ARCHIVE_IMPORT_FOLDER_LINK,
        t::folder_link_fmt(&folder),
        true,
        Gesture::Settings(Action::OpenFolders),
    ));
    els.push(back_button());
    els
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SubPage;
    use fauna_archive_import_machine::{ArchiveSummaryView, CategoryRow, RunState};

    fn a_snapshot(step: ArchiveImportStep) -> ArchiveImportSnapshot {
        let mut s = ArchiveImportSnapshot::empty();
        s.step = step;
        s
    }

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

    fn rows() -> Vec<CategoryRow> {
        vec![
            CategoryRow {
                token: "posts".into(),
                count: 4,
                importable: true,
                selected: true,
                imported: 1,
                skipped: 0,
            },
            CategoryRow {
                token: "events".into(),
                count: 3,
                importable: true,
                selected: false,
                imported: 0,
                skipped: 0,
            },
            CategoryRow {
                token: "friends".into(),
                count: 2,
                importable: false,
                selected: false,
                imported: 0,
                skipped: 0,
            },
        ]
    }

    fn state_with(snapshot: ArchiveImportSnapshot) -> SettingsState {
        let mut state = SettingsState {
            sub: SubPage::ArchiveImport,
            ..Default::default()
        };
        state.archive_import.snapshot = Some(snapshot);
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

    fn attr_of(els: &[Element], id: &str, index: usize, key: &str) -> String {
        els.iter()
            .filter(|e| e.id == id)
            .nth(index)
            .and_then(|e| {
                e.attrs
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| v.clone())
            })
            .unwrap_or_default()
    }

    /// Every static ui.yaml id for the `archive-import` page paints, collected
    /// across the six steps — the `mail_import.rs` analogue.
    #[test]
    fn every_static_ui_yaml_id_paints_across_the_six_steps() {
        let mut painted: Vec<String> = Vec::new();
        painted.extend(ids(&archive_import_elements(&state_with(a_snapshot(
            ArchiveImportStep::Source,
        )))));
        let mut archive = a_snapshot(ArchiveImportStep::Archive);
        archive.summary = Some(summary());
        painted.extend(ids(&archive_import_elements(&state_with(archive))));
        let mut scope = a_snapshot(ArchiveImportStep::Scope);
        scope.summary = Some(summary());
        scope.categories = rows();
        scope.nest_supports_hidden_tiers = Some(false);
        painted.extend(ids(&archive_import_elements(&state_with(scope))));
        painted.extend(ids(&archive_import_elements(&state_with(a_snapshot(
            ArchiveImportStep::Confirm,
        )))));
        let mut progress = a_snapshot(ArchiveImportStep::Progress);
        progress.categories = rows();
        progress.run_state = Some(RunState::Running);
        painted.extend(ids(&archive_import_elements(&state_with(progress))));
        let mut done = a_snapshot(ArchiveImportStep::Done);
        done.folder_name = Some("Facebook archive 2026-09-08".into());
        done.run_state = Some(RunState::Completed);
        let mut done_state = state_with(done);
        done_state.archive_import.show_skipped = true;
        painted.extend(ids(&archive_import_elements(&done_state)));
        for id in [
            "page-heading",
            // "error-message" is registered globally by ui::register_frame.
            "archive-import-source-picker",
            "archive-import-source-help",
            "archive-import-archive-path",
            "archive-import-archive-open-button",
            "archive-import-archive-summary",
            "archive-import-scope-categories",
            "archive-import-scope-category-item",
            "archive-import-scope-audience-mode",
            "archive-import-scope-audience-summary",
            "archive-import-scope-date-from",
            "archive-import-scope-date-to",
            "wizard-next-button",
            "wizard-back-button",
            "archive-import-confirm-summary",
            "archive-import-start-button",
            "archive-import-progress-summary",
            "archive-import-progress-bar",
            "archive-import-pause-button",
            "archive-import-resume-button",
            "archive-import-cancel-button",
            "archive-import-error-log",
            "archive-import-category-progress-list",
            "archive-import-category-progress-list-item",
            "archive-import-category-progress-list-item-name",
            "archive-import-category-progress-list-item-progress",
            "archive-import-done-summary",
            "archive-import-view-imported-button",
            "archive-import-review-skipped-button",
            "archive-import-profile-prefill-button",
            "archive-import-folder-link",
            "settings-nav-back",
        ] {
            assert!(
                painted.contains(&id.to_string()),
                "missing {id:?}; painted {painted:?}"
            );
        }
    }

    #[test]
    fn each_step_paints_only_its_own_ids() {
        let source = ids(&archive_import_elements(&state_with(a_snapshot(
            ArchiveImportStep::Source,
        ))));
        assert!(source.iter().any(|id| id == "archive-import-source-picker"));
        assert!(!source.iter().any(|id| id == "archive-import-archive-path"));
        assert!(!source.iter().any(|id| id == "archive-import-start-button"));
        let scope = ids(&archive_import_elements(&state_with(a_snapshot(
            ArchiveImportStep::Scope,
        ))));
        assert!(
            scope
                .iter()
                .any(|id| id == "archive-import-scope-audience-mode")
        );
        assert!(!scope.iter().any(|id| id == "archive-import-source-picker"));
        let done = ids(&archive_import_elements(&state_with(a_snapshot(
            ArchiveImportStep::Done,
        ))));
        assert!(
            done.iter()
                .any(|id| id == "archive-import-view-imported-button")
        );
        assert!(!done.iter().any(|id| id == "archive-import-pause-button"));
    }

    #[test]
    fn pre_hydrate_the_wizard_opens_on_source_with_facebook_selected() {
        let state = SettingsState {
            sub: SubPage::ArchiveImport,
            ..Default::default()
        };
        let els = archive_import_elements(&state);
        assert_eq!(
            text_of(&els, "archive-import-source-picker", 0),
            source_kind_label(ArchiveSourceKind::Facebook)
        );
        assert_eq!(
            text_of(&els, "archive-import-source-help", 0),
            t::SOURCE_HELP_FACEBOOK
        );
    }

    #[test]
    fn an_unavailable_machine_paints_the_reason_and_no_wizard() {
        let mut state = SettingsState {
            sub: SubPage::ArchiveImport,
            ..Default::default()
        };
        state.archive_import.unavailable = Some("no sync device id".into());
        let els = archive_import_elements(&state);
        assert!(els.iter().any(|e| e.text == t::UNAVAILABLE));
        assert!(!ids(&els).contains(&"archive-import-source-picker".to_string()));
    }

    #[test]
    fn the_archive_summary_renders_dates_sizes_and_the_undated_form() {
        let mut snap = a_snapshot(ArchiveImportStep::Archive);
        snap.summary = Some(summary());
        let text = text_of(
            &archive_import_elements(&state_with(snap)),
            "archive-import-archive-summary",
            0,
        );
        assert!(
            text.starts_with("Facebook · Test Owner · 2020-01-26 – 2022-04-15 · "),
            "{text}"
        );
        let mut undated = a_snapshot(ArchiveImportStep::Archive);
        undated.summary = Some(ArchiveSummaryView {
            first_at: 0,
            last_at: 0,
            ..summary()
        });
        let text = text_of(
            &archive_import_elements(&state_with(undated)),
            "archive-import-archive-summary",
            0,
        );
        assert!(text.contains("no dated records"), "{text}");
        let none = archive_import_elements(&state_with(a_snapshot(ArchiveImportStep::Archive)));
        assert!(
            !ids(&none).contains(&"archive-import-archive-summary".to_string()),
            "no summary before an archive is indexed"
        );
    }

    #[test]
    fn scope_rows_carry_their_state_and_a_kept_category_is_not_a_checkbox() {
        let mut snap = a_snapshot(ArchiveImportStep::Scope);
        snap.categories = rows();
        let els = archive_import_elements(&state_with(snap));
        assert_eq!(
            attr_of(&els, "archive-import-scope-category-item", 0, "state"),
            "on"
        );
        assert_eq!(
            attr_of(&els, "archive-import-scope-category-item", 1, "state"),
            "off"
        );
        assert_eq!(
            attr_of(&els, "archive-import-scope-category-item", 2, "state"),
            "kept"
        );
        assert_eq!(
            text_of(&els, "archive-import-scope-category-item", 0),
            t::scope_category_row_fmt(t::CATEGORY_POSTS, "4")
        );
        assert_eq!(
            text_of(&els, "archive-import-scope-category-item", 2),
            t::scope_category_kept_fmt(t::CATEGORY_FRIENDS, "2")
        );
        let kept = els
            .iter()
            .filter(|e| e.id == "archive-import-scope-category-item")
            .nth(2)
            .unwrap();
        assert!(!matches!(kept.role, crate::element::Role::Checkbox { .. }));
    }

    #[test]
    fn the_audience_summary_states_known_vs_unknown_and_only_me_overrides_it() {
        let mut snap = a_snapshot(ArchiveImportStep::Scope);
        snap.summary = Some(summary());
        let els = archive_import_elements(&state_with(snap.clone()));
        assert_eq!(
            text_of(&els, "archive-import-scope-audience-summary", 0),
            t::scope_audience_summary_fmt("3", "2")
        );
        snap.audience_mode = AudienceMode::OnlyMe;
        let els = archive_import_elements(&state_with(snap));
        assert_eq!(
            text_of(&els, "archive-import-scope-audience-summary", 0),
            t::SCOPE_AUDIENCE_SUMMARY_ONLY_ME
        );
    }

    #[test]
    fn a_nest_without_hidden_tiers_is_said_on_the_scope_step() {
        let mut snap = a_snapshot(ArchiveImportStep::Scope);
        snap.nest_supports_hidden_tiers = Some(false);
        let els = archive_import_elements(&state_with(snap));
        assert!(
            els.iter()
                .any(|e| e.text == t::SCOPE_HIDDEN_TIERS_UNAVAILABLE)
        );
        let mut ok = a_snapshot(ArchiveImportStep::Scope);
        ok.nest_supports_hidden_tiers = Some(true);
        assert!(
            !archive_import_elements(&state_with(ok))
                .iter()
                .any(|e| e.text == t::SCOPE_HIDDEN_TIERS_UNAVAILABLE)
        );
    }

    #[test]
    fn the_confirm_summary_counts_records_and_bytes() {
        let mut snap = a_snapshot(ArchiveImportStep::Confirm);
        snap.confirm_records = 5;
        snap.confirm_bytes = 4_158;
        let text = text_of(
            &archive_import_elements(&state_with(snap)),
            "archive-import-confirm-summary",
            0,
        );
        assert!(text.starts_with("5 records · about "), "{text}");
    }

    #[test]
    fn the_progress_bar_uses_the_shared_quota_fraction_over_settled_records() {
        let mut snap = a_snapshot(ArchiveImportStep::Progress);
        snap.total = 4;
        snap.imported = 2;
        snap.skipped = 1;
        snap.run_state = Some(RunState::Running);
        let els = archive_import_elements(&state_with(snap));
        assert_eq!(text_of(&els, "archive-import-progress-bar", 0), "75%");
        assert_eq!(
            text_of(&els, "archive-import-progress-summary", 0),
            t::progress_summary_fmt(t::STATE_RUNNING, "2", "4", "1")
        );
    }

    #[test]
    fn progress_rows_paint_flat_for_selected_categories_only() {
        let mut snap = a_snapshot(ArchiveImportStep::Progress);
        snap.categories = rows();
        let els = archive_import_elements(&state_with(snap));
        assert_eq!(
            els.iter()
                .filter(|e| e.id == "archive-import-category-progress-list-item")
                .count(),
            1
        );
        assert_eq!(
            text_of(&els, "archive-import-category-progress-list-item-name", 0),
            t::CATEGORY_POSTS
        );
        assert_eq!(
            text_of(
                &els,
                "archive-import-category-progress-list-item-progress",
                0
            ),
            t::progress_row_fmt("1", "4", "0")
        );
        assert!(
            els.iter().all(|e| e.path.is_empty()),
            "every leaf on this page paints flat"
        );
    }

    #[test]
    fn the_error_log_renders_the_snapshots_lines() {
        let mut snap = a_snapshot(ArchiveImportStep::Progress);
        snap.skip_log = vec![
            "posts: outside the date range".into(),
            "events: calendar not enabled".into(),
        ];
        let els = archive_import_elements(&state_with(snap));
        assert_eq!(
            text_of(&els, "archive-import-error-log", 0),
            "posts: outside the date range\nevents: calendar not enabled"
        );
    }

    #[test]
    fn the_cancel_button_relabels_when_armed() {
        let mut state = state_with(a_snapshot(ArchiveImportStep::Progress));
        let unarmed = text_of(
            &archive_import_elements(&state),
            "archive-import-cancel-button",
            0,
        );
        state.archive_import.cancel_armed = true;
        let armed = text_of(
            &archive_import_elements(&state),
            "archive-import-cancel-button",
            0,
        );
        assert_ne!(unarmed, armed);
        assert_eq!(armed, fauna_i18n::strings::common::CONFIRM_Q);
    }

    #[test]
    fn back_leaves_a_progress_screen_only_once_the_run_has_concluded() {
        for (run_state, expected) in [
            (RunState::Running, false),
            (RunState::Paused, false),
            (RunState::Cancelled, true),
            (RunState::Completed, true),
        ] {
            let mut snap = a_snapshot(ArchiveImportStep::Progress);
            snap.run_state = Some(run_state);
            let has_back = ids(&archive_import_elements(&state_with(snap)))
                .contains(&"wizard-back-button".to_string());
            assert_eq!(has_back, expected, "{run_state:?}");
        }
    }

    #[test]
    fn the_done_step_buttons_do_what_they_say() {
        let mut snap = a_snapshot(ArchiveImportStep::Done);
        snap.folder_name = Some("Facebook archive 2026-09-08".into());
        snap.skip_log = vec!["posts: already imported".into()];
        let mut state = state_with(snap);
        let els = archive_import_elements(&state);
        let find = |els: &[Element], id: &str| els.iter().find(|e| e.id == id).cloned().expect(id);
        assert!(matches!(
            find(&els, "archive-import-view-imported-button").role,
            crate::element::Role::Button(Gesture::Nav(crate::pages::Page::Feed))
        ));
        assert!(matches!(
            find(&els, "archive-import-folder-link").role,
            crate::element::Role::Button(Gesture::Settings(Action::OpenFolders))
        ));
        assert_eq!(
            find(&els, "archive-import-folder-link").text,
            t::folder_link_fmt("Facebook archive 2026-09-08")
        );
        assert!(
            !find(&els, "archive-import-profile-prefill-button").enabled,
            "offered, never applied silently — and not built yet"
        );
        assert!(
            !ids(&els).contains(&"archive-import-error-log".to_string()),
            "the skip log is behind Review skipped"
        );
        state.archive_import.show_skipped = true;
        let els = archive_import_elements(&state);
        assert_eq!(
            text_of(&els, "archive-import-error-log", 0),
            "posts: already imported"
        );
    }

    #[test]
    fn the_done_summary_renders_through_the_i18n_template() {
        let mut snap = a_snapshot(ArchiveImportStep::Done);
        snap.imported = 10;
        snap.skipped = 1;
        let els = archive_import_elements(&state_with(snap));
        assert_eq!(
            text_of(&els, "archive-import-done-summary", 0),
            t::done_summary_fmt("10", "1")
        );
    }

    #[test]
    fn open_actions_commit_the_trimmed_path_then_open() {
        let s = ArchiveImportState {
            path_input: "  C:\\exports\\facebook.zip ".into(),
            ..ArchiveImportState::default()
        };
        assert_eq!(
            s.open_actions(),
            vec![
                ArchiveImportAction::SetArchivePath {
                    value: "C:\\exports\\facebook.zip".into()
                },
                ArchiveImportAction::OpenArchive,
            ]
        );
    }

    #[test]
    fn advancing_off_scope_commits_both_dates_before_next() {
        let s = ArchiveImportState {
            date_from_input: " 2020-01-01 ".into(),
            date_to_input: "".into(),
            ..ArchiveImportState::default()
        };
        assert_eq!(
            s.scope_next_actions(),
            vec![
                ArchiveImportAction::SetDateFrom {
                    value: "2020-01-01".into()
                },
                ArchiveImportAction::SetDateTo { value: "".into() },
                ArchiveImportAction::Next,
            ]
        );
    }

    #[test]
    fn a_fresh_visit_disarms_cancel_and_drops_every_draft() {
        let mut s = ArchiveImportState {
            cancel_armed: true,
            show_skipped: true,
            path_input: "x".into(),
            date_from_input: "2020-01-01".into(),
            date_to_input: "2021-01-01".into(),
            ..ArchiveImportState::default()
        };
        s.reset_form();
        assert!(!s.cancel_armed && !s.show_skipped);
        assert!(
            s.path_input.is_empty() && s.date_from_input.is_empty() && s.date_to_input.is_empty()
        );
    }

    #[test]
    fn the_two_pickers_round_trip_their_labels() {
        for kind in SOURCE_KINDS {
            assert_eq!(source_kind_for_label(&source_kind_label(kind)), kind);
        }
        for mode in AUDIENCE_MODES {
            assert_eq!(audience_mode_for_label(&audience_mode_label(mode)), mode);
        }
        assert_eq!(
            source_kind_for_label("MySpace"),
            ArchiveSourceKind::Facebook
        );
    }
}
