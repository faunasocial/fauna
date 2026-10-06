//! The Settings → Mail → Aliases sub-page (`docs/goal/behavior/mail-aliases.md`
//! § Aliases UX — the `mail-aliases` page; `tests/e2e-unified/ui.yaml`
//! `mail-aliases` + its `mail-aliases-list` / `mail-aliases-import-sheet`
//! components).
//!
//! A person manages **their own** extra mail addresses here: the canonical
//! `<handle>@<domain>` primary, extra exact aliases, wildcard prefixes, and
//! one-click disposable mints — each with its own label, spam-threshold
//! override, rate cap and on/off switch.
//!
//! **A dumb renderer over the shared machine.** Every decision is
//! `fauna_client_mail_settings::MailAliasesMachine`'s: the projection, the
//! pattern validators, the `default_domain` derivation, the per-line import
//! outcomes. This module paints `MailAliasesSnapshot` and dispatches
//! `MailAliasesAction`, exactly as `mail-aliases.md` § Architectural rules
//! requires and as the six apps before it do. The two display formatters are
//! shared too — `alias_kind_badge` (the seven-arm kind→i18n map) and
//! `alias_hits_label` (the count + optional-last-hit template); tui is the
//! **third direct-Rust consumer** after linux, so no FFI hop and no shared-Rust
//! work was owed. linux (`apps/fauna-linux/src/settings/mail_aliases.rs`) is the
//! reference leg; tui is the seventh and last app to lift it (priority #1).
//!
//! **The error bridge is load-bearing.** `machine.dispatch(..)`'s `Result` is
//! deliberately ignored at the call site (linux does the same): a dispatch
//! failure arrives on the *snapshot* (`MailAliasesSnapshot.error`), not the
//! return value. The fold copies it onto `App::errors` — without it a rejected
//! create (`conflicts_with_existing_alias`, `reserved_local_part`) would paint
//! no error and have no effect, the dropped-command shape testing.md point 11
//! forbids.
//!
//! **Row controls are `.within(ids::MAIL_ALIASES_LIST_ITEM, i)`-scoped, and the
//! canonical row omits them.** The canonical row renders none of the four
//! mutating controls, so their own occurrence-index compresses relative to
//! the row's pattern-index the moment a canonical row precedes the target —
//! a plain `click(id, index)` would then hit the row *after* the intended one
//! (the index-space bug `actions/mail_aliases.py::revoke`/`delete`/
//! `toggle_active` now fix by scoping to the row itself, mirroring
//! `family-ward-item` / `muted-word-item`). A nested indexed list that skips
//! the containment declaration silently reads empty in every scoped query
//! (the A6 nesting lesson, `family.rs`). The canonical `<handle>@<domain>`
//! row renders **read-only** (no toggle/edit/revoke/overflow, a
//! primary-address badge instead), because the nest rejects
//! disabling/renaming/deleting it (`canonical_alias_protected`) and
//! `mail-aliases.md` § Aliases UX wants that visible up-front rather than as
//! an error on attempt. `test_mail_aliases_canonical_readonly.py` pins it as
//! "exactly `n-1` of each control for `n` rows". Every control's *dispatch*
//! still carries its row's `alias_id_hex`, never an index — only the e2e
//! agent's element lookup is index-scoped, so a list that re-orders under a
//! fresh snapshot can still never revoke the wrong alias in production.

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_client_mail_settings::{
    AliasKind, AliasView, ImportAliasStatusView, MailAliasesAction, MailAliasesMachine,
    MailAliasesSnapshot, alias_hits_label, alias_kind_badge,
};
use fauna_i18n::strings::mail_aliases as t;

use super::{Action, SettingsField, SettingsState};
use crate::element::{Element, Field, Gesture};

/// The Aliases sub-page's state.
///
/// The machine is built once at the post-auth hook (`attach_session`), the
/// `MailState` shape: construction is sync and cheap (the RPC is `hydrate()`),
/// it carries interior mutability, and holding it as an `Arc` is what lets an
/// `Op` own only `Arc`s and cross a `tokio::spawn`. Session-scoped —
/// `clear_session` drops it, so a stale machine never outlives a sign-out.
#[derive(Default)]
pub struct MailAliasesState {
    /// The shared machine. `None` pre-login.
    pub machine: Option<Arc<MailAliasesMachine>>,
    /// The last snapshot the page painted. `None` until the nav-edge hydrate
    /// folds one — the list then paints its honest empty state.
    pub snapshot: Option<MailAliasesSnapshot>,

    // ── the add/edit sheet (an inline reveal, not a modal — the
    // `mail-add-credential` idiom every app uses; the state protocol cannot
    // open a separate window) ──
    /// Whether the add/edit sheet is open.
    pub show_add_form: bool,
    /// `Some(alias_id_hex)` = the sheet is in **edit** mode for that row
    /// (submit dispatches `Update`); `None` = add mode (submit dispatches
    /// `Create`). The kind picker is read-only while editing — a wildcard cannot
    /// become a disposable mid-life (`mail-aliases.md` § Aliases UX).
    pub editing: Option<String>,
    /// The kind picker's value (`mail-aliases.md` § Layout: Exact / Wildcard
    /// prefix / Disposable). Each click steps to the next in that order, so
    /// `actions/mail_aliases.py::add_wildcard` still reaches Wildcard with one
    /// click and a disposable takes two.
    pub sheet_kind: SheetKind,
    /// `mail-aliases-add-sheet-pattern-input` — the localpart / wildcard prefix.
    /// A local draft committed only on submit; the shared machine validates it.
    pub pattern_input: String,
    /// `mail-aliases-add-sheet-label-input` (optional).
    pub label_input: String,
    /// `mail-aliases-add-sheet-spam-threshold-input` (optional, 0–15). Parsed at
    /// submit; an unparseable entry is simply `None`, matching linux.
    pub spam_threshold_input: String,
    /// `mail-aliases-add-sheet-rate-per-hour-input` (optional).
    pub rate_per_hour_input: String,
    /// `mail-aliases-add-sheet-ttl-input` — how many days a disposable lasts,
    /// shown only while the picker reads Disposable and sent on its
    /// `GenerateDisposable` (empty = the per-user default). The android shape
    /// (`MailAliasesScreen.kt`), lifted.
    pub ttl_input: String,
    /// `mail-aliases-add-sheet-uses-input` — how many messages a disposable
    /// takes before it expires; the twin of [`Self::ttl_input`].
    pub uses_input: String,

    // ── the bulk paste-import sheet ──
    /// Whether `mail-aliases-import-sheet` is open. Mutually exclusive with the
    /// add/edit sheet (linux's `open_import_sheet` hides the other).
    pub show_import_form: bool,
    /// `mail-aliases-import-textarea` — one address per line.
    pub import_input: String,
    /// Whether a submit has happened since the sheet was last opened, gating
    /// `mail-aliases-import-result`. The convention every leg follows is that
    /// the sheet **stays open on submit** and renders the outcome in place,
    /// and that the result is **cleared when the sheet reopens** so a stale
    /// batch's summary never greets the next paste (`mail-aliases.md`
    /// § Implementation status). The machine clears `last_import_result` only on
    /// the *next dispatch*, and reopening the sheet is not one — so this local
    /// flag is what makes the reopen honest.
    pub import_result_shown: bool,

    // ── two-click inline confirms (no modal; ui.yaml scopes no separate
    // confirm id to this page, so the arm lives on the button itself) ──
    /// The `alias_id_hex` whose revoke button is armed (first click relabels,
    /// second dispatches `Revoke`).
    pub revoke_armed: Option<String>,
    /// The `alias_id_hex` whose overflow-menu Delete is armed. Delete is
    /// destructive and irreversible, so it gets the same two-click gate.
    pub delete_armed: Option<String>,

    /// The full address the last disposable mint copied to the clipboard —
    /// the confirmation `mail-aliases.md` § Layout asks for ("copied to the
    /// clipboard with a toast"). Set from the snapshot that carries
    /// `last_minted_address` and replaced by the next one to fold, so it
    /// lasts until the person's next gesture on the page (a terminal has no
    /// timer-dismissed toast; linux's lasts 4 s).
    pub minted_copied: Option<String>,
}

/// The add sheet's kind choice (`mail-aliases.md` § Layout — the kind picker's
/// three options). Narrower than the shared `AliasKind`, which also names the
/// kinds a row can show but the sheet never creates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SheetKind {
    #[default]
    Exact,
    Wildcard,
    Disposable,
}

impl SheetKind {
    /// The next option a picker click selects.
    pub(super) fn next(self) -> Self {
        match self {
            Self::Exact => Self::Wildcard,
            Self::Wildcard => Self::Disposable,
            Self::Disposable => Self::Exact,
        }
    }

    /// The sheet kind an existing row reopens as (its kind is fixed on edit).
    fn of(kind: AliasKind) -> Self {
        match kind {
            AliasKind::Wildcard => Self::Wildcard,
            AliasKind::Disposable => Self::Disposable,
            _ => Self::Exact,
        }
    }

    /// The picker's `kind` attr — the selection a test reads.
    fn attr(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Wildcard => "wildcard",
            Self::Disposable => "disposable",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Exact => t::KIND_EXACT,
            Self::Wildcard => t::KIND_WILDCARD,
            Self::Disposable => t::KIND_DISPOSABLE,
        }
    }
}

impl MailAliasesState {
    /// Build the page's shared machine from the session's WS handle. Infallible
    /// (unlike Mail, which decodes a secret) — the seam is user-tier and derives
    /// the owning actor nest-side.
    pub(super) fn build(nest: Arc<fauna_client::NestClient>) -> Self {
        Self {
            machine: Some(Arc::new(
                fauna_client_mail_settings::rpc_glue::build_mail_aliases_machine(nest),
            )),
            ..Self::default()
        }
    }

    /// Drop every page-local draft on a fresh visit — the snapshot survives (the
    /// nav-edge hydrate replaces it). Mirrors `MailState::reset_form`, so a
    /// half-typed pattern or an armed destructive confirm never survives a
    /// nav-away and fires against a later visit's list.
    pub(super) fn reset_form(&mut self) {
        self.show_add_form = false;
        self.editing = None;
        self.sheet_kind = SheetKind::Exact;
        self.pattern_input.clear();
        self.label_input.clear();
        self.spam_threshold_input.clear();
        self.rate_per_hour_input.clear();
        self.ttl_input.clear();
        self.uses_input.clear();
        self.show_import_form = false;
        self.import_input.clear();
        self.import_result_shown = false;
        self.revoke_armed = None;
        self.delete_armed = None;
        self.minted_copied = None;
    }

    /// Open the add sheet on an empty form (add mode).
    pub(super) fn open_add_form(&mut self) {
        self.reset_form();
        self.show_add_form = true;
    }

    /// Open the sheet pre-populated from `view` (edit mode). The kind is
    /// immutable here, so the picker paints the row's kind and is disabled.
    pub(super) fn open_edit_form(&mut self, view: &AliasView) {
        self.reset_form();
        self.show_add_form = true;
        self.editing = Some(view.alias_id_hex.clone());
        self.sheet_kind = SheetKind::of(view.kind);
        self.pattern_input = view.pattern.clone();
        self.label_input = view.label.clone();
        self.spam_threshold_input = view
            .spam_threshold_override
            .map(|v| v.to_string())
            .unwrap_or_default();
        self.rate_per_hour_input = view
            .rate_limit_per_hour
            .map(|v| v.to_string())
            .unwrap_or_default();
    }

    /// Open the bulk-import sheet on an empty textarea + a cleared result
    /// (linux's `open_import_sheet`).
    pub(super) fn open_import_form(&mut self) {
        self.reset_form();
        self.show_import_form = true;
    }

    /// The row behind an `alias_id_hex`, for the gestures that need its fields.
    pub(super) fn alias(&self, alias_id_hex: &str) -> Option<&AliasView> {
        self.snapshot
            .as_ref()?
            .aliases
            .iter()
            .find(|a| a.alias_id_hex == alias_id_hex)
    }

    /// The add/edit sheet's current values as a `Create`/`Update` action, or
    /// `None` when there is no `default_domain` to create on (mail not enabled).
    ///
    /// Parsing is deliberately lenient — an unparseable optional numeric is
    /// `None`, never an error — because the shared machine and then the nest are
    /// the authorities on what a valid alias is; this layer only collects.
    ///
    /// Lenient about *unparseable*, though, never about **parseable-but-invalid**:
    /// both optionals go through the shared validators
    /// (`value-formatting.md` § Mail-knob validation), which is what keeps a
    /// negative out of `rate_limit_per_hour`. A bare `.parse::<i64>().ok()` here
    /// accepted `-5` and sent a negative hourly limit to the nest — the exact
    /// per-app drift `parse_count_i64`'s `>= 0` filter exists to eliminate. The
    /// spam sibling needs no dedicated fn (§ Mail-knob validation says so): it
    /// consumes `parse_count`, whose `u32` parse rejects a negative by itself.
    pub(super) fn submit_action(&self) -> Option<MailAliasesAction> {
        let pattern = self.pattern_input.trim().to_string();
        let label = self.label_input.trim().to_string();
        let spam_threshold_override = fauna_core::format::parse_count(&self.spam_threshold_input);
        let rate_limit_per_hour = fauna_core::format::parse_count_i64(&self.rate_per_hour_input);
        match &self.editing {
            Some(alias_id_hex) => Some(MailAliasesAction::Update {
                alias_id_hex: alias_id_hex.clone(),
                pattern,
                label,
                spam_threshold_override,
                rate_limit_per_hour,
            }),
            None => {
                // Create needs a domain; the caller disables the affordance when
                // there is none, so this is the belt to that braces.
                self.snapshot.as_ref()?.default_domain.as_ref()?;
                Some(match self.sheet_kind {
                    // A disposable's address is minted, never typed: the sheet
                    // sends its lifetime, use count and label (empty fields =
                    // the per-user defaults).
                    SheetKind::Disposable => MailAliasesAction::GenerateDisposable {
                        ttl_days: fauna_core::format::parse_count(&self.ttl_input),
                        uses: fauna_core::format::parse_count(&self.uses_input),
                        label,
                    },
                    kind => MailAliasesAction::Create {
                        kind: if kind == SheetKind::Wildcard {
                            AliasKind::Wildcard
                        } else {
                            AliasKind::Exact
                        },
                        pattern,
                        label,
                        spam_threshold_override,
                        rate_limit_per_hour,
                    },
                })
            }
        }
    }
}

/// `mail-aliases-list-item-hits` — the shared `alias_hits_label` template with
/// the last-hit date formatted natively.
///
/// The shared fn owns the surrounding template (count, the "· last {date}"
/// arm); the calendar date depends on the viewer's **local** timezone, so it is
/// passed in pre-formatted, the same split windows/apple/linux/web/android use.
/// A terminal has no locale-aware date widget, so this is the plain `%Y-%m-%d`
/// local date the rest of the tui renders (`crate::format::epoch_secs_date`).
fn hits_text(view: &AliasView) -> String {
    let last = view
        .last_hit_at_ms
        .filter(|ms| *ms > 0)
        .map(|ms| crate::format::epoch_secs_date((ms / 1000).max(0) as u64));
    crate::wizard::localized(&alias_hits_label(view.hit_count, last))
}

/// `mail-aliases-import-result` — the shared `ImportResultView` counts through
/// the `mail_aliases.import_result` template, plus one
/// `mail_aliases.import_invalid_line` row per invalid outcome.
///
/// The per-invalid *reason* rows are not optional: `mail-aliases.md` § Bulk
/// import requires the reason be rendered, not just tallied, and every other
/// app renders them through this same shared string.
fn import_result_text(snapshot: &MailAliasesSnapshot) -> Option<String> {
    let result = snapshot.last_import_result.as_ref()?;
    let mut text = t::import_result(
        &result.created.to_string(),
        &result.skipped_duplicate.to_string(),
        &result.invalid.to_string(),
    );
    for outcome in &result.outcomes {
        if outcome.status == ImportAliasStatusView::Invalid {
            text.push('\n');
            text.push_str(&t::import_invalid_line(
                &outcome.address,
                outcome.reason.as_deref().unwrap_or(""),
            ));
        }
    }
    Some(text)
}

/// `mail-aliases-generate-disposable-button`, carrying the just-copied address
/// as its `copied` attr after a mint so a test asserts the CONTENTS of the
/// confirmation (OSC 52 is fire-and-forget; the `account-actor-id-copy-btn`
/// copy-button contract).
fn generate_button(a: &MailAliasesState, has_domain: bool) -> Element {
    let button = Element::gesture_button(
        ids::MAIL_ALIASES_GENERATE_DISPOSABLE_BUTTON,
        t::GENERATE_BUTTON,
        has_domain,
        Gesture::Settings(Action::MailAliasesGenerateDisposable),
    );
    match &a.minted_copied {
        Some(address) => button.attr("copied", address.clone()),
        None => button,
    }
}

/// The Aliases sub-page's ordered element list.
///
/// The page's `error-message` is registered globally by
/// [`crate::ui::register_frame`] (the `tui-settings`/`logs`/`account` precedent),
/// so it is not painted here.
pub(super) fn mail_aliases_elements(state: &SettingsState) -> Vec<Element> {
    let a = &state.mail_aliases;
    let snapshot = a.snapshot.as_ref();
    // Every create path (add-sheet Create, the disposable mint, and the bulk
    // import, which creates exact aliases too) needs a domain to create on. The
    // machine derives it from the canonical exact alias; without mail enabled
    // there is none and the nest would reject with `no_canonical_address`, so
    // the affordances are disabled and the page says why (linux's `has_domain`).
    let has_domain = snapshot.is_some_and(|s| s.default_domain.is_some());

    let mut els = vec![
        Element::label(ids::PAGE_HEADING, t::TITLE),
        Element::chrome(t::DESCRIPTION),
        Element::gesture_button(
            ids::MAIL_ALIASES_ADD_BUTTON,
            t::ADD_BUTTON,
            has_domain,
            Gesture::Settings(Action::MailAliasesOpenAdd),
        ),
        Element::gesture_button(
            ids::MAIL_ALIASES_IMPORT_BUTTON,
            t::IMPORT_BUTTON,
            has_domain,
            Gesture::Settings(Action::MailAliasesOpenImport),
        ),
        generate_button(a, has_domain),
    ];
    // The mint's copy confirmation, beside the button that minted it.
    if let Some(address) = &a.minted_copied {
        els.push(Element::chrome(format!("{} {address}", t::COPIED)));
    }
    // The honest reason the three affordances above are dead, when the snapshot
    // has resolved and carries no error of its own to show instead.
    //
    // ⚠ Two conditions, two messages (`ui/README.md` rule 5 Q2 — the
    // `error_no_set` / `error_no_sync_set` precedent). `has_domain` is false
    // both *before* the hydrate resolves and *after* it resolves with mail
    // off, and the user's next act differs: wait, versus go enable mail. The
    // pre-hydrate line rides below with the list, where it also displaces the
    // false "No aliases yet".
    if snapshot.is_some() && !has_domain {
        els.push(Element::chrome(t::NO_DEFAULT_DOMAIN));
    }

    // ── the add/edit sheet ──
    if a.show_add_form {
        let editing = a.editing.is_some();
        els.push(Element::chrome(if editing {
            t::EDIT
        } else {
            t::FORM_TITLE
        }));
        // Read-only on edit — a wildcard cannot become a disposable mid-life
        // (`mail-aliases.md` § Layout) — and painted so, not just inert.
        // A click steps Exact → Wildcard → Disposable; the `kind` attr carries
        // the selection (ui.yaml: "Selection exposed via attribute").
        els.push(
            Element::checkbox_gesture(
                ids::MAIL_ALIASES_ADD_SHEET_KIND_PICKER,
                a.sheet_kind.label(),
                a.sheet_kind != SheetKind::Exact,
                Gesture::Settings(Action::MailAliasesToggleKind),
            )
            .attr("kind", a.sheet_kind.attr())
            .enabled(!editing),
        );
        // Minting a disposable: its address is generated, so there is no
        // pattern to type, and the mint takes a lifetime, a use count and a
        // label — the per-alias controls are not part of it, so they are not
        // offered either (`mail-aliases.md` § Layout).
        let minting = !editing && a.sheet_kind == SheetKind::Disposable;
        if !minting {
            els.push(
                Element::input(
                    ids::MAIL_ALIASES_ADD_SHEET_PATTERN_INPUT,
                    a.pattern_input.clone(),
                    Field::Settings(SettingsField::MailAliasPattern),
                )
                .labelled(t::PATTERN_PLACEHOLDER),
            );
        }
        els.push(
            Element::input(
                ids::MAIL_ALIASES_ADD_SHEET_LABEL_INPUT,
                a.label_input.clone(),
                Field::Settings(SettingsField::MailAliasLabel),
            )
            .labelled(t::LABEL_PLACEHOLDER),
        );
        if minting {
            // The disposable-only pair (ui.yaml: "Disposable-only").
            els.push(
                Element::input(
                    ids::MAIL_ALIASES_ADD_SHEET_TTL_INPUT,
                    a.ttl_input.clone(),
                    Field::Settings(SettingsField::MailAliasTtl),
                )
                .labelled(t::TTL_PLACEHOLDER),
            );
            els.push(
                Element::input(
                    ids::MAIL_ALIASES_ADD_SHEET_USES_INPUT,
                    a.uses_input.clone(),
                    Field::Settings(SettingsField::MailAliasUses),
                )
                .labelled(t::USES_PLACEHOLDER),
            );
        } else {
            els.push(
                Element::input(
                    ids::MAIL_ALIASES_ADD_SHEET_SPAM_THRESHOLD_INPUT,
                    a.spam_threshold_input.clone(),
                    Field::Settings(SettingsField::MailAliasSpamThreshold),
                )
                .labelled(t::SPAM_THRESHOLD_PLACEHOLDER),
            );
            els.push(
                Element::input(
                    ids::MAIL_ALIASES_ADD_SHEET_RATE_PER_HOUR_INPUT,
                    a.rate_per_hour_input.clone(),
                    Field::Settings(SettingsField::MailAliasRatePerHour),
                )
                .labelled(t::RATE_PER_HOUR_PLACEHOLDER),
            );
        }
        els.push(Element::gesture_button(
            ids::MAIL_ALIASES_ADD_SHEET_SUBMIT_BUTTON,
            t::SUBMIT,
            // Edit needs no domain (the row already has one); create does.
            editing || has_domain,
            Gesture::Settings(Action::MailAliasesSubmit { editing }),
        ));
        els.push(Element::gesture_button(
            ids::MAIL_ALIASES_ADD_SHEET_CANCEL_BUTTON,
            t::CANCEL,
            true,
            Gesture::Settings(Action::MailAliasesCancelForm),
        ));
    }

    // ── the bulk paste-import sheet ──
    if a.show_import_form {
        els.push(Element::chrome(t::IMPORT_TITLE));
        els.push(Element::chrome(t::IMPORT_SUBTITLE));
        els.push(
            Element::input(
                ids::MAIL_ALIASES_IMPORT_TEXTAREA,
                a.import_input.clone(),
                Field::Settings(SettingsField::MailAliasImport),
            )
            .labelled(t::IMPORT_PLACEHOLDER),
        );
        els.push(Element::gesture_button(
            ids::MAIL_ALIASES_IMPORT_SUBMIT_BUTTON,
            t::IMPORT_SUBMIT,
            has_domain,
            Gesture::Settings(Action::MailAliasesImportSubmit),
        ));
        els.push(Element::gesture_button(
            ids::MAIL_ALIASES_IMPORT_CANCEL_BUTTON,
            t::IMPORT_CANCEL,
            true,
            Gesture::Settings(Action::MailAliasesCancelForm),
        ));
        // Stays open on submit and renders the outcome in place; the local flag
        // is what keeps a previous batch's summary from greeting a fresh paste.
        if a.import_result_shown
            && let Some(text) = snapshot.and_then(import_result_text)
        {
            els.push(Element::label(ids::MAIL_ALIASES_IMPORT_RESULT, text));
        }
    }

    // ── the alias list ──
    // An un-hydrated page must not assert "No aliases yet" — it does not know
    // yet, and the claim is worse than silence: it reads as a settled fact
    // while all three create affordances sit dead above it with no reason.
    // The loading line is both the honest empty-state and rule 5's reason for
    // that window (`ui/README.md` rule 5).
    let aliases: &[AliasView] = snapshot.map(|s| s.aliases.as_slice()).unwrap_or(&[]);
    if snapshot.is_none() {
        els.push(Element::chrome(t::LOADING));
    } else if aliases.is_empty() {
        els.push(Element::chrome(t::EMPTY));
    }
    for (i, view) in aliases.iter().enumerate() {
        els.extend(alias_row_elements(a, i, view));
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

/// One row, nested `.within(ids::MAIL_ALIASES_LIST_ITEM, i)` — see the module
/// docs for why (the index-space bug this closes) and why the canonical row
/// omits the four mutating controls.
fn alias_row_elements(a: &MailAliasesState, i: usize, view: &AliasView) -> Vec<Element> {
    const ITEM: &str = ids::MAIL_ALIASES_LIST_ITEM;
    let id = &view.alias_id_hex;
    let kind_label = crate::wizard::localized(&alias_kind_badge(view.kind));
    let mut els = vec![
        Element::label(ITEM, view.address.clone()),
        Element::label(ids::MAIL_ALIASES_LIST_ITEM_PATTERN, view.address.clone()).within(ITEM, i),
        Element::label(ids::MAIL_ALIASES_LIST_ITEM_KIND, kind_label).within(ITEM, i),
        Element::label(ids::MAIL_ALIASES_LIST_ITEM_LABEL, view.label.clone()).within(ITEM, i),
        Element::label(ids::MAIL_ALIASES_LIST_ITEM_HITS, hits_text(view)).within(ITEM, i),
    ];

    if view.is_canonical {
        // Read-only: the primary-address marker instead of the four mutating
        // controls. An untagged label — ui.yaml mints no id for it, and
        // inventing one would be the invented-ID anti-pattern.
        els.push(Element::chrome(t::PRIMARY_ADDRESS_BADGE).within(ITEM, i));
    } else {
        els.push(
            Element::checkbox_gesture(
                ids::MAIL_ALIASES_LIST_ITEM_DISABLED_TOGGLE,
                t::ACTIVE_TOGGLE_LABEL,
                // Two-way and labelled "Active": ON = receiving. Checked is
                // the *enabled* state, so the element's own value is
                // `!disabled`, and the click will move it the other way — so
                // the dispatched action's `enabling` is `view.disabled`.
                !view.disabled,
                Gesture::Settings(Action::MailAliasesToggleActive {
                    alias_id_hex: id.clone(),
                    enabling: view.disabled,
                }),
            )
            .within(ITEM, i),
        );
        els.push(
            Element::gesture_button(
                ids::MAIL_ALIASES_LIST_ITEM_EDIT_BUTTON,
                t::EDIT,
                true,
                Gesture::Settings(Action::MailAliasesOpenEdit(id.clone())),
            )
            .within(ITEM, i),
        );
        els.push(
            Element::gesture_button(
                ids::MAIL_ALIASES_LIST_ITEM_REVOKE_BUTTON,
                // The armed state relabels in place — the two-click confirm's
                // only visible affordance, since ui.yaml scopes no confirm id
                // here.
                if a.revoke_armed.as_deref() == Some(id.as_str()) {
                    fauna_i18n::strings::common::CONFIRM_Q
                } else {
                    t::REVOKE
                },
                true,
                Gesture::Settings(Action::MailAliasesRevoke(id.clone())),
            )
            .within(ITEM, i),
        );
        els.push(
            Element::gesture_button(
                ids::MAIL_ALIASES_LIST_ITEM_OVERFLOW_MENU,
                if a.delete_armed.as_deref() == Some(id.as_str()) {
                    fauna_i18n::strings::common::CONFIRM_Q
                } else {
                    t::DELETE
                },
                true,
                Gesture::Settings(Action::MailAliasesDelete(id.clone())),
            )
            .within(ITEM, i),
        );
    }

    // The audit disclosure is inert on every app: `list_account_alias_hits`
    // landed nest-side but has no consumer in the shared `MailAliasesMachine`
    // yet (`mail-aliases.md` § Still deferred). Rendered for parity — wiring it
    // is a fleet-wide slice through the shared machine, not a tui one.
    els.push(Element::label(ids::MAIL_ALIASES_LIST_ITEM_SHOW_AUDIT, t::SHOW_AUDIT).within(ITEM, i));
    els
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SubPage;
    use fauna_client_mail_settings::AliasesStatus;

    /// One construction site for the fixture, so a field added to the shared
    /// `AliasView` lands here and nowhere else. This is the struct-update
    /// fixture convention that keeps two branches independently growing the same
    /// wire type from colliding on the grown axis; `AliasView` has no `Default`
    /// to spread, so a single builder is the equivalent.
    fn alias(id: &str, pattern: &str, kind: AliasKind) -> AliasView {
        AliasView {
            alias_id_hex: id.to_string(),
            local_domain: "example.test".to_string(),
            kind,
            pattern: pattern.to_string(),
            address: format!("{pattern}@example.test"),
            label: String::new(),
            disabled: false,
            is_canonical: false,
            hit_count: 0,
            last_hit_at_ms: None,
            spam_threshold_override: None,
            rate_limit_per_hour: None,
            rate_limit_per_day: None,
            uses_remaining: None,
            expires_at_ms: None,
        }
    }

    fn snapshot(aliases: Vec<AliasView>, default_domain: Option<&str>) -> MailAliasesSnapshot {
        MailAliasesSnapshot {
            aliases,
            default_domain: default_domain.map(|d| d.to_string()),
            last_minted_address: None,
            last_import_result: None,
            status: AliasesStatus::Idle,
            error: None,
        }
    }

    fn state_with(aliases: Vec<AliasView>, default_domain: Option<&str>) -> SettingsState {
        let mut state = SettingsState {
            sub: SubPage::MailAliases,
            ..Default::default()
        };
        state.mail_aliases.snapshot = Some(snapshot(aliases, default_domain));
        state
    }

    fn ids(els: &[Element]) -> Vec<String> {
        els.iter().map(|e| e.id.clone()).collect()
    }

    fn count(els: &[Element], id: &str) -> usize {
        els.iter().filter(|e| e.id == id).count()
    }

    fn find<'a>(els: &'a [Element], id: &str) -> &'a Element {
        els.iter()
            .find(|e| e.id == id)
            .unwrap_or_else(|| panic!("missing {id:?}; have {:?}", ids(els)))
    }

    /// The page's three always-on affordances render even with no data at all —
    /// the state a fresh visit paints before its hydrate resolves.
    #[test]
    fn the_empty_page_paints_every_static_ui_yaml_id() {
        let els = mail_aliases_elements(&SettingsState::default());
        for id in [
            "page-heading",
            "mail-aliases-add-button",
            "mail-aliases-import-button",
            "mail-aliases-generate-disposable-button",
            "settings-nav-back",
        ] {
            assert!(
                ids(&els).contains(&id.to_string()),
                "missing {id:?}; have {:?}",
                ids(&els)
            );
        }
        assert_eq!(
            count(&els, "mail-aliases-list-item"),
            0,
            "an un-hydrated page must paint no rows"
        );
    }

    /// One row per alias, every leaf present, rendered in snapshot order.
    #[test]
    fn a_populated_list_paints_one_row_per_alias_with_every_leaf() {
        let els = mail_aliases_elements(&state_with(
            vec![
                alias("a1", "shop", AliasKind::Exact),
                alias("a2", "news", AliasKind::Wildcard),
            ],
            Some("example.test"),
        ));
        for id in [
            "mail-aliases-list-item",
            "mail-aliases-list-item-pattern",
            "mail-aliases-list-item-kind",
            "mail-aliases-list-item-label",
            "mail-aliases-list-item-hits",
            "mail-aliases-list-item-disabled-toggle",
            "mail-aliases-list-item-edit-button",
            "mail-aliases-list-item-revoke-button",
            "mail-aliases-list-item-overflow-menu",
            "mail-aliases-list-item-show-audit",
        ] {
            assert_eq!(count(&els, id), 2, "expected one {id:?} per row");
        }
        let patterns: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "mail-aliases-list-item-pattern")
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(patterns, vec!["shop@example.test", "news@example.test"]);
    }

    /// The load-bearing canonical contract, and the exact shape
    /// `test_mail_aliases_canonical_readonly.py` asserts cross-app: for `n`
    /// rows where one is canonical, each mutating control appears exactly `n-1`
    /// times, while every row still renders its read-only leaves.
    #[test]
    fn the_canonical_row_omits_all_four_mutating_controls() {
        let mut canonical = alias("a0", "bob", AliasKind::Exact);
        canonical.is_canonical = true;
        let els = mail_aliases_elements(&state_with(
            vec![
                canonical,
                alias("a1", "shop", AliasKind::Exact),
                alias("a2", "news", AliasKind::Wildcard),
            ],
            Some("example.test"),
        ));
        let n = count(&els, "mail-aliases-list-item-pattern");
        assert_eq!(n, 3);
        for control in [
            "mail-aliases-list-item-disabled-toggle",
            "mail-aliases-list-item-edit-button",
            "mail-aliases-list-item-revoke-button",
            "mail-aliases-list-item-overflow-menu",
        ] {
            assert_eq!(
                count(&els, control),
                n - 1,
                "exactly the canonical row must omit {control:?}"
            );
        }
        // The read-only leaves and the audit disclosure stay on every row.
        for id in [
            "mail-aliases-list-item-kind",
            "mail-aliases-list-item-hits",
            "mail-aliases-list-item-show-audit",
        ] {
            assert_eq!(count(&els, id), n, "{id:?} belongs on every row");
        }
    }

    /// Row controls carry their alias's id, never its index — so a list that
    /// re-orders under a fresh snapshot can never revoke the wrong alias.
    #[test]
    fn row_controls_carry_the_alias_id_not_the_row_index() {
        let els = mail_aliases_elements(&state_with(
            vec![
                alias("aaaa", "shop", AliasKind::Exact),
                alias("bbbb", "news", AliasKind::Exact),
            ],
            Some("example.test"),
        ));
        let revokes: Vec<&Element> = els
            .iter()
            .filter(|e| e.id == "mail-aliases-list-item-revoke-button")
            .collect();
        assert!(matches!(
            revokes[1].role,
            crate::element::Role::Button(Gesture::Settings(Action::MailAliasesRevoke(ref id)))
                if id == "bbbb"
        ));
    }

    /// The switch is two-way and paints the ENABLED state, so a disabled alias
    /// shows an off switch — the "not a one-way trap" contract.
    #[test]
    fn the_active_toggle_paints_the_enabled_state() {
        let mut off = alias("a1", "shop", AliasKind::Exact);
        off.disabled = true;
        let els = mail_aliases_elements(&state_with(
            vec![off, alias("a2", "news", AliasKind::Exact)],
            Some("example.test"),
        ));
        let toggles: Vec<bool> = els
            .iter()
            .filter(|e| e.id == "mail-aliases-list-item-disabled-toggle")
            .map(|e| matches!(e.role, crate::element::Role::Checkbox { checked, .. } if checked))
            .collect();
        assert_eq!(
            toggles,
            vec![false, true],
            "checked == receiving; a disabled alias paints OFF"
        );
    }

    /// Without a `default_domain` there is nothing to create on, so all three
    /// create affordances are dead — and the page says why rather than failing
    /// at the nest with `no_canonical_address`.
    #[test]
    fn no_default_domain_disables_every_create_affordance() {
        let els = mail_aliases_elements(&state_with(vec![], None));
        for id in [
            "mail-aliases-add-button",
            "mail-aliases-import-button",
            "mail-aliases-generate-disposable-button",
        ] {
            assert!(!find(&els, id).enabled, "{id} must be disabled");
        }
        assert!(
            els.iter().any(|e| e.text == t::NO_DEFAULT_DOMAIN),
            "the page must say why the create affordances are dead"
        );
        // …and live again once a domain resolves.
        let els = mail_aliases_elements(&state_with(vec![], Some("example.test")));
        for id in [
            "mail-aliases-add-button",
            "mail-aliases-import-button",
            "mail-aliases-generate-disposable-button",
        ] {
            assert!(find(&els, id).enabled, "{id} must be enabled");
        }
    }

    /// Rule 5 in the window the resolved-state reason above cannot cover: with
    /// no snapshot yet, the same three affordances are dead for a *different*
    /// reason, and the page must neither stay silent nor assert "No aliases
    /// yet" — a claim it has no basis for until the hydrate lands.
    #[test]
    fn the_un_hydrated_page_says_it_is_loading_instead_of_claiming_no_aliases() {
        let els = mail_aliases_elements(&SettingsState::default());
        for id in [
            "mail-aliases-add-button",
            "mail-aliases-import-button",
            "mail-aliases-generate-disposable-button",
        ] {
            assert!(!find(&els, id).enabled, "{id} must be disabled pre-hydrate");
        }
        assert!(
            els.iter().any(|e| e.text == t::LOADING),
            "an un-hydrated page must say why its create affordances are dead"
        );
        assert!(
            !els.iter().any(|e| e.text == t::EMPTY),
            "an un-hydrated page must NOT claim the user has no aliases"
        );
        // Once the hydrate resolves empty, the settled claim is the right one.
        let els = mail_aliases_elements(&state_with(vec![], Some("example.test")));
        assert!(els.iter().any(|e| e.text == t::EMPTY));
        assert!(!els.iter().any(|e| e.text == t::LOADING));
    }

    /// The add sheet renders the full ui.yaml element set across its kinds:
    /// Exact/Wildcard paint the pattern and the per-alias controls, Disposable
    /// the ttl/uses pair its mint takes, and no pattern (the address is
    /// generated).
    #[test]
    fn the_open_add_sheet_paints_every_sheet_id() {
        let mut state = state_with(vec![], Some("example.test"));
        state.mail_aliases.open_add_form();
        let els = mail_aliases_elements(&state);
        for id in [
            "mail-aliases-add-sheet-kind-picker",
            "mail-aliases-add-sheet-pattern-input",
            "mail-aliases-add-sheet-label-input",
            "mail-aliases-add-sheet-spam-threshold-input",
            "mail-aliases-add-sheet-rate-per-hour-input",
            "mail-aliases-add-sheet-submit-button",
            "mail-aliases-add-sheet-cancel-button",
        ] {
            assert!(
                ids(&els).contains(&id.to_string()),
                "missing {id:?}; have {:?}",
                ids(&els)
            );
        }
        assert!(!ids(&els).contains(&"mail-aliases-add-sheet-ttl-input".to_string()));

        state.mail_aliases.sheet_kind = SheetKind::Disposable;
        let els = mail_aliases_elements(&state);
        for id in [
            "mail-aliases-add-sheet-ttl-input",
            "mail-aliases-add-sheet-uses-input",
            "mail-aliases-add-sheet-label-input",
        ] {
            assert!(
                ids(&els).contains(&id.to_string()),
                "a disposable sheet must paint {id:?}; have {:?}",
                ids(&els)
            );
        }
        assert!(
            !ids(&els).contains(&"mail-aliases-add-sheet-pattern-input".to_string()),
            "a disposable's address is generated, not typed"
        );
        assert_eq!(
            find(&els, "mail-aliases-add-sheet-kind-picker")
                .attrs
                .iter()
                .find(|(k, _)| k == "kind")
                .map(|(_, v)| v.as_str()),
            Some("disposable")
        );
        // Closed by default — the sheet is an inline reveal, not the page.
        let els = mail_aliases_elements(&state_with(vec![], Some("example.test")));
        assert!(!ids(&els).contains(&"mail-aliases-add-sheet-pattern-input".to_string()));
    }

    /// The kind is immutable on an existing alias, so the edit sheet paints
    /// its picker disabled — read-only, not merely inert — while the add sheet
    /// keeps it live.
    #[test]
    fn the_kind_picker_is_read_only_on_edit_only() {
        let row = alias("a1", "shop", AliasKind::Exact);
        let mut state = state_with(vec![row.clone()], Some("example.test"));
        state.mail_aliases.open_add_form();
        let els = mail_aliases_elements(&state);
        assert!(find(&els, "mail-aliases-add-sheet-kind-picker").enabled);

        state.mail_aliases.open_edit_form(&row);
        let els = mail_aliases_elements(&state);
        assert!(!find(&els, "mail-aliases-add-sheet-kind-picker").enabled);
    }

    /// A mint's copy is confirmed on the page, and the generate button carries
    /// the copied address as its `copied` attr; the next fresh visit drops it.
    #[test]
    fn a_mint_confirms_the_copied_address() {
        let mut state = state_with(vec![], Some("example.test"));
        let els = mail_aliases_elements(&state);
        let button = find(&els, "mail-aliases-generate-disposable-button");
        assert!(button.attrs.iter().all(|(k, _)| k != "copied"));

        let address = "bob-temp-AbCdEf@example.test";
        state.mail_aliases.minted_copied = Some(address.to_string());
        let els = mail_aliases_elements(&state);
        let button = find(&els, "mail-aliases-generate-disposable-button");
        assert!(
            button
                .attrs
                .contains(&("copied".to_string(), address.to_string())),
            "{:?}",
            button.attrs
        );
        assert!(
            els.iter()
                .any(|e| e.text == format!("{} {address}", t::COPIED)),
            "the page must confirm the copy in words"
        );

        state.mail_aliases.reset_form();
        let els = mail_aliases_elements(&state);
        assert!(!els.iter().any(|e| e.text.starts_with(t::COPIED)));
    }

    /// Add mode builds `Create` with the picked kind; edit mode builds `Update`
    /// carrying the row's id, and the optional numerics parse leniently.
    #[test]
    fn submit_builds_create_in_add_mode_and_update_in_edit_mode() {
        let mut state = state_with(vec![alias("a1", "shop", AliasKind::Exact)], Some("d.test"));
        state.mail_aliases.open_add_form();
        state.mail_aliases.pattern_input = "news-".into();
        state.mail_aliases.sheet_kind = SheetKind::Wildcard;
        state.mail_aliases.spam_threshold_input = "9".into();
        state.mail_aliases.rate_per_hour_input = "not a number".into();
        match state.mail_aliases.submit_action() {
            Some(MailAliasesAction::Create {
                kind,
                pattern,
                spam_threshold_override,
                rate_limit_per_hour,
                ..
            }) => {
                assert_eq!(kind, AliasKind::Wildcard);
                assert_eq!(pattern, "news-");
                assert_eq!(spam_threshold_override, Some(9));
                assert_eq!(
                    rate_limit_per_hour, None,
                    "an unparseable optional is unset, never an error"
                );
            }
            other => panic!("expected Create, got {other:?}"),
        }

        let view = alias("a1", "shop", AliasKind::Exact);
        state.mail_aliases.open_edit_form(&view);
        match state.mail_aliases.submit_action() {
            Some(MailAliasesAction::Update { alias_id_hex, .. }) => {
                assert_eq!(alias_id_hex, "a1")
            }
            other => panic!("expected Update, got {other:?}"),
        }
    }

    /// A **negative** rate cap is not a lenient `None` — it is rejected outright,
    /// exactly as the shared validator rejects it.
    ///
    /// "Parsing is deliberately lenient" (this sheet's own doc comment) means an
    /// *unparseable* optional reads as unset; it never meant a **parseable but
    /// invalid** value gets to ride the wire. `-5` parses fine as an `i64`, so the
    /// hand-rolled `.parse::<i64>().ok()` here sent a negative hourly rate limit
    /// to the nest — the precise "natives' incidental `long`-parse drift" that
    /// `fauna_core::format::parse_count_i64` was written to eliminate
    /// (`value-formatting.md` § Mail-knob validation). Both spellings of unset —
    /// negative and unparseable — must land on `None`, and for the same reason.
    #[test]
    fn a_negative_rate_cap_is_rejected_like_the_shared_validator_rejects_it() {
        let mut state = state_with(vec![alias("a1", "shop", AliasKind::Exact)], Some("d.test"));
        state.mail_aliases.open_add_form();
        state.mail_aliases.pattern_input = "news".into();

        for (input, expected) in [
            ("-5", None),
            ("not a number", None),
            ("2.5", None),
            ("  12  ", Some(12)),
            ("+7", Some(7)),
            ("0", Some(0)),
        ] {
            state.mail_aliases.rate_per_hour_input = input.into();
            let Some(MailAliasesAction::Create {
                rate_limit_per_hour,
                ..
            }) = state.mail_aliases.submit_action()
            else {
                panic!("expected Create for input {input:?}");
            };
            assert_eq!(
                rate_limit_per_hour, expected,
                "input {input:?} must agree with the shared parse_count_i64"
            );
            assert_eq!(
                rate_limit_per_hour,
                fauna_core::format::parse_count_i64(input),
                "input {input:?} must BE the shared parse_count_i64"
            );
        }
    }

    /// The picker steps Exact → Wildcard → Disposable → Exact, and a
    /// Disposable submit mints with the typed lifetime, use count and label
    /// (`mail-aliases.md` § Layout); empty or unparseable fields send `None`,
    /// the per-user defaults.
    #[test]
    fn a_disposable_sheet_mints_with_its_lifetime_and_uses() {
        assert_eq!(SheetKind::Exact.next(), SheetKind::Wildcard);
        assert_eq!(SheetKind::Wildcard.next(), SheetKind::Disposable);
        assert_eq!(SheetKind::Disposable.next(), SheetKind::Exact);

        let mut state = state_with(vec![], Some("d.test"));
        state.mail_aliases.open_add_form();
        state.mail_aliases.sheet_kind = SheetKind::Disposable;
        state.mail_aliases.ttl_input = "3".into();
        state.mail_aliases.uses_input = "1".into();
        state.mail_aliases.label_input = "Shop".into();
        match state.mail_aliases.submit_action() {
            Some(MailAliasesAction::GenerateDisposable {
                ttl_days,
                uses,
                label,
            }) => {
                assert_eq!((ttl_days, uses), (Some(3), Some(1)));
                assert_eq!(label, "Shop");
            }
            other => panic!("expected GenerateDisposable, got {other:?}"),
        }

        state.mail_aliases.ttl_input.clear();
        state.mail_aliases.uses_input = "many".into();
        match state.mail_aliases.submit_action() {
            Some(MailAliasesAction::GenerateDisposable { ttl_days, uses, .. }) => {
                assert_eq!((ttl_days, uses), (None, None));
            }
            other => panic!("expected GenerateDisposable, got {other:?}"),
        }
    }

    /// Edit mode seeds the sheet from the row and pins the kind — a wildcard
    /// cannot become a disposable mid-life.
    #[test]
    fn edit_mode_seeds_the_sheet_and_pins_the_kind() {
        let mut view = alias("a1", "news-", AliasKind::Wildcard);
        view.label = "Newsletters".into();
        view.spam_threshold_override = Some(7);
        let mut state = state_with(vec![view.clone()], Some("example.test"));
        state.mail_aliases.open_edit_form(&view);
        assert_eq!(state.mail_aliases.pattern_input, "news-");
        assert_eq!(state.mail_aliases.label_input, "Newsletters");
        assert_eq!(state.mail_aliases.spam_threshold_input, "7");
        assert_eq!(state.mail_aliases.sheet_kind, SheetKind::Wildcard);
        assert_eq!(state.mail_aliases.editing.as_deref(), Some("a1"));
    }

    /// The import sheet's result is gated on a submit having happened since the
    /// sheet was opened, so a previous batch's summary never greets a fresh
    /// paste (the "cleared when the sheet reopens" convention).
    #[test]
    fn the_import_result_only_paints_after_a_submit_in_this_opening() {
        use fauna_client_mail_settings::ImportResultView;
        let mut state = state_with(vec![], Some("example.test"));
        if let Some(s) = state.mail_aliases.snapshot.as_mut() {
            s.last_import_result = Some(ImportResultView {
                created: 2,
                skipped_duplicate: 1,
                invalid: 0,
                outcomes: Vec::new(),
            });
        }
        state.mail_aliases.open_import_form();
        let els = mail_aliases_elements(&state);
        assert!(
            ids(&els).contains(&"mail-aliases-import-textarea".to_string()),
            "the sheet itself must paint"
        );
        assert!(
            !ids(&els).contains(&"mail-aliases-import-result".to_string()),
            "a freshly-opened sheet must not show the previous batch's summary"
        );

        state.mail_aliases.import_result_shown = true;
        let els = mail_aliases_elements(&state);
        let result = find(&els, "mail-aliases-import-result");
        assert!(
            result.text.contains('2') && result.text.contains('1'),
            "the shared counts template must render; got {:?}",
            result.text
        );
    }

    /// Arming relabels the button in place — the two-click confirm's only
    /// visible affordance, and what the driver's adaptive two-click drives.
    #[test]
    fn arming_a_destructive_control_relabels_it_in_place() {
        let mut state = state_with(vec![alias("a1", "shop", AliasKind::Exact)], Some("d.test"));
        let els = mail_aliases_elements(&state);
        assert_eq!(
            find(&els, "mail-aliases-list-item-revoke-button").text,
            t::REVOKE
        );
        assert_eq!(
            find(&els, "mail-aliases-list-item-overflow-menu").text,
            t::DELETE
        );

        state.mail_aliases.revoke_armed = Some("a1".into());
        let els = mail_aliases_elements(&state);
        assert_eq!(
            find(&els, "mail-aliases-list-item-revoke-button").text,
            fauna_i18n::strings::common::CONFIRM_Q
        );
        // Arming revoke must not arm delete too.
        assert_eq!(
            find(&els, "mail-aliases-list-item-overflow-menu").text,
            t::DELETE
        );
    }

    /// A fresh visit drops every draft and disarms every destructive confirm —
    /// so a half-typed pattern, or a primed delete, can never survive a nav-away
    /// and fire against a later visit's list.
    #[test]
    fn reset_form_drops_every_draft_and_disarms_the_confirms() {
        let mut a = MailAliasesState {
            show_add_form: true,
            editing: Some("a1".into()),
            sheet_kind: SheetKind::Disposable,
            pattern_input: "half-typed".into(),
            label_input: "l".into(),
            show_import_form: true,
            import_input: "x@y.test".into(),
            import_result_shown: true,
            revoke_armed: Some("a1".into()),
            delete_armed: Some("a2".into()),
            ..MailAliasesState::default()
        };
        a.reset_form();
        assert!(!a.show_add_form && !a.show_import_form);
        assert!(a.editing.is_none() && a.sheet_kind == SheetKind::Exact);
        assert!(a.pattern_input.is_empty() && a.label_input.is_empty());
        assert!(a.import_input.is_empty() && !a.import_result_shown);
        assert!(a.revoke_armed.is_none() && a.delete_armed.is_none());
    }

    /// The hits text routes through the shared `alias_hits_label`, and a
    /// never-hit alias takes its count-only arm rather than rendering an epoch.
    #[test]
    fn hits_text_uses_the_shared_label_and_omits_a_never_hit_date() {
        let mut view = alias("a1", "shop", AliasKind::Exact);
        view.hit_count = 4;
        assert_eq!(hits_text(&view), t::hits("4"));
        view.last_hit_at_ms = Some(1_700_000_000_000);
        let with_date = hits_text(&view);
        assert!(
            with_date.contains('4') && with_date.len() > t::hits("4").len(),
            "the with-date arm must render the shared two-part template; got {with_date:?}"
        );
    }
}
