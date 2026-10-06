//! The user-facing "Aliases" preferences page (linux; the mail-UX seed's lead
//! app).
//!
//! Where a person manages **their own** per-account mail addresses: the
//! canonical `<handle>@<domain>` exact alias minted at mail-enable, extra exact
//! aliases, a wildcard prefix, and one-click disposable mints — each with an
//! optional label, a per-alias spam-threshold / rate-limit override, and
//! disable / revoke / delete controls. Target behavior: `docs/goal/behavior/
//! mail-aliases.md` § Aliases UX. UX/IDs: `tests/e2e-unified/ui.yaml`
//! (`mail-aliases` page + `mail-aliases-list` component).
//!
//! Per `mail-aliases.md` § Where logic lives this layer holds **no** business
//! logic — it is a dumb renderer of [`MailAliasesSnapshot`] + dispatcher of
//! [`MailAliasesAction`]; the projection, validators, default-domain derivation,
//! and action sequencing all live in the shared
//! `fauna_client_mail_settings::aliases` machine (priority #2/#4), the prior art
//! the other five apps lift. Direct sibling: `settings/mail.rs`.
//!
//! # AT-SPI discoverability + inline reveals
//!
//! Same idiom as `mail.rs`: read-only fields carry a 1px marker `gtk::Label`
//! tagged with `set_test_id`; directly-actuable controls (`gtk::Entry`,
//! `gtk::Button`, `gtk::CheckButton`) carry the ID on the widget itself. The
//! add/edit sheet, the bulk paste-import sheet, and the per-row Delete confirm
//! are **inline reveals** (not modals) — the linux state protocol can't open the
//! separate `adw::PreferencesWindow`, so the whole page is embedded in the
//! status view (`views/status.rs`) and an inline form stays inside that
//! reachable tree.
//!
//! # Bulk paste-import
//!
//! `mail-aliases-import-button` reveals `mail-aliases-import-sheet`: a
//! multi-line `gtk::TextView` (one address per line) →
//! [`MailAliasesAction::Import`] → the per-line outcome summary in
//! `mail-aliases-import-result` (`mail-aliases.md` § Bulk import). Structurally
//! this mirrors `settings/mail_list_members.rs`'s import sheet — the same
//! TextView-in-a-ScrolledWindow + submit/cancel shape, which `ui.yaml`'s
//! `mail-aliases-import-sheet` component explicitly points at ("Mirrors
//! mail-list-members-import").
//!
//! # Re-enable is wired; one honest gap remains
//!
//! - **Disable / re-enable**: the per-row "Active" toggle is **two-way** —
//!   turning it off revokes (`disabled=true`), turning it back on re-enables
//!   (`fauna.bridges.enable_account_alias`, `disabled=false`). Disable is no
//!   longer a one-way trap (`mail-aliases.md:156`). The nest rejects disabling
//!   or deleting the **canonical** `<handle>@<domain>` alias
//!   (`canonical_alias_protected`), surfaced as an error.
//! - **Show-audit** (gap): `mail-aliases-list-item-show-audit` is rendered
//!   inert — the per-alias hit list (`list_account_alias_hits`) is a follow-on
//!   slice; the machine doesn't expose it yet.
//! - **Canonical read-only row** (follow-on): the canonical row should render
//!   read-only (no toggle/delete, marked "primary") once the `is_canonical`
//!   wire flag lands; today the nest guard prevents the damage and surfaces the
//!   error.

use fauna_ui_ids as ids;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

use fauna_client_mail_settings::{
    AliasKind, AliasView, ImportAliasStatusView, MailAliasesAction, MailAliasesMachine,
    MailAliasesSnapshot,
};

use crate::async_helper::spawn_with_snapshot;
use crate::i18n::strings::mail_aliases as S;
use crate::testid::set_test_id;

/// Which dispatch the add/edit sheet's submit maps to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FormMode {
    /// New alias → `Create` (kind from the picker).
    Add,
    /// Edit an existing alias → `Update` (kind immutable, picker read-only).
    Edit,
}

/// Handles to the widgets the snapshot renders into. GTK objects are
/// reference-counted, so cloning this is cheap and shares the same widgets.
#[derive(Clone)]
struct AliasesWidgets {
    error_label: gtk::Label,

    // Manage group (add + import + generate buttons).
    add_button: gtk::Button,
    import_button: gtk::Button,
    generate_button: gtk::Button,
    minted_toast: gtk::Label,

    // Aliases list group + dynamic rows.
    list_group: adw::PreferencesGroup,
    list_placeholder: adw::ActionRow,
    rows: Rc<RefCell<Vec<adw::ActionRow>>>,

    // Add/edit inline sheet.
    sheet_group: adw::PreferencesGroup,
    kind_picker: gtk::CheckButton,
    pattern_input: gtk::Entry,
    label_input: gtk::Entry,
    spam_threshold_input: gtk::Entry,
    rate_per_hour_input: gtk::Entry,
    // ttl/uses inputs are disposable-only and minted via the generate button,
    // not this sheet, so the seed never reads them — they live in the widget
    // tree (for ui.yaml conformance) but not in this struct.
    submit_button: gtk::Button,
    cancel_button: gtk::Button,

    // Bulk paste-import inline sheet (mail-aliases-import-sheet).
    import_sheet: adw::PreferencesGroup,
    import_input: gtk::TextView,
    import_submit: gtk::Button,
    import_cancel: gtk::Button,
    import_result: gtk::Label,
}

/// Everything the page's handlers + render need. `Rc`-shared into every closure.
struct AliasesCtx {
    machine: Arc<MailAliasesMachine>,
    rt: tokio::runtime::Handle,
    form_mode: Cell<FormMode>,
    /// `Some(hex)` while the sheet is editing that row (Edit mode).
    editing_id: RefCell<Option<String>>,
    w: AliasesWidgets,
}

/// Build the "Aliases" preferences page, plus the on-visible refresh closure
/// the settings shell calls on the navigation edge (mirrors
/// `build_mail_lists_page`).
pub fn build_mail_aliases_page() -> (adw::PreferencesPage, Rc<dyn Fn()>) {
    let page = adw::PreferencesPage::builder()
        .title(S::TITLE)
        .icon_name("mail-message-new-symbolic")
        .build();

    // --- Top group: heading + page-level error + add/generate buttons ---
    let top_group = adw::PreferencesGroup::builder()
        .title(S::TITLE)
        .description(S::DESCRIPTION)
        .build();
    top_group.set_header_suffix(Some(&super::marker("page-heading")));

    // error-message — page-level error label (Rule 2), hidden until set.
    let error_label = gtk::Label::builder().visible(false).build();
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    let error_row = adw::ActionRow::builder().activatable(false).build();
    error_row.add_suffix(&error_label);
    top_group.add(&error_row);

    // mail-aliases-add-button — opens the add sheet.
    let add_button = gtk::Button::builder()
        .label(S::ADD_BUTTON)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(&add_button, ids::MAIL_ALIASES_ADD_BUTTON);
    let add_row = adw::ActionRow::builder().activatable(false).build();
    add_row.add_suffix(&add_button);
    top_group.add(&add_row);

    // mail-aliases-import-button — opens the bulk paste-import sheet
    // (`mail-aliases.md` § Bulk import: the recipient-whitelist import path —
    // paste ~100 approved addresses in one shot instead of one dialog each).
    let import_button = gtk::Button::builder()
        .label(S::IMPORT_BUTTON)
        .valign(gtk::Align::Center)
        .build();
    set_test_id(&import_button, ids::MAIL_ALIASES_IMPORT_BUTTON);
    let import_row = adw::ActionRow::builder().activatable(false).build();
    import_row.add_suffix(&import_button);
    top_group.add(&import_row);

    // mail-aliases-generate-disposable-button — one-click disposable mint.
    let generate_button = gtk::Button::builder()
        .label(S::GENERATE_BUTTON)
        .valign(gtk::Align::Center)
        .build();
    set_test_id(
        &generate_button,
        ids::MAIL_ALIASES_GENERATE_DISPOSABLE_BUTTON,
    );
    crate::offline_gate::declare_wire_kind(
        &generate_button,
        "fauna.bridges.generate_disposable_alias",
    );
    let generate_row = adw::ActionRow::builder().activatable(false).build();
    generate_row.add_suffix(&generate_button);
    // A transient "copied <address>" confirmation after a successful mint.
    let minted_toast = gtk::Label::builder()
        .label("")
        .visible(false)
        .css_classes(["dim-label"])
        .build();
    generate_row.add_suffix(&minted_toast);
    top_group.add(&generate_row);
    page.add(&top_group);

    // --- Aliases list group ---
    // mail-aliases-list — the group container. Per-alias rows
    // (mail-aliases-list-item*) are rebuilt from MailAliasesSnapshot.aliases on
    // every render().
    let list_group = adw::PreferencesGroup::builder().title(S::TITLE).build();
    list_group.set_header_suffix(Some(&super::marker("mail-aliases-list")));
    // Un-hydrated first paint must not claim "No aliases yet" — the page does
    // not know that yet, and the claim is worse than silence while every
    // create affordance sits dead above it with no reason (`ui/README.md`
    // rule 5). `render()` below overwrites the title once a real snapshot
    // lands.
    let list_placeholder = adw::ActionRow::builder().title(S::LOADING).build();
    list_group.add(&list_placeholder);
    page.add(&list_group);

    // --- Add/edit inline sheet (hidden) ---
    let sheet_group = adw::PreferencesGroup::builder()
        .title(S::FORM_TITLE)
        .build();
    sheet_group.set_visible(false);
    let sheet_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    sheet_box.set_margin_top(8);
    sheet_box.set_margin_bottom(8);

    // mail-aliases-add-sheet-kind-picker — value-via-state kind selector. A
    // single native gtk::CheckButton with one stable id (same idiom as
    // mail-add-credential-type-selector): unchecked = Exact (the default),
    // checked = Wildcard prefix. Disposable mints via the dedicated generate
    // button, not this sheet (mail-aliases.md § Aliases UX). Read-only on Edit.
    let kind_picker = gtk::CheckButton::with_label(S::KIND_WILDCARD_LABEL);
    set_test_id(&kind_picker, ids::MAIL_ALIASES_ADD_SHEET_KIND_PICKER);
    sheet_box.append(&kind_picker);

    // mail-aliases-add-sheet-pattern-input.
    let pattern_input = gtk::Entry::builder()
        .placeholder_text(S::PATTERN_PLACEHOLDER)
        .build();
    set_test_id(&pattern_input, ids::MAIL_ALIASES_ADD_SHEET_PATTERN_INPUT);
    sheet_box.append(&pattern_input);

    // mail-aliases-add-sheet-label-input (optional).
    let label_input = gtk::Entry::builder()
        .placeholder_text(S::LABEL_PLACEHOLDER)
        .build();
    set_test_id(&label_input, ids::MAIL_ALIASES_ADD_SHEET_LABEL_INPUT);
    sheet_box.append(&label_input);

    // mail-aliases-add-sheet-spam-threshold-input (optional, 0–15).
    let spam_threshold_input = gtk::Entry::builder()
        .placeholder_text(S::SPAM_THRESHOLD_PLACEHOLDER)
        .build();
    set_test_id(
        &spam_threshold_input,
        ids::MAIL_ALIASES_ADD_SHEET_SPAM_THRESHOLD_INPUT,
    );
    sheet_box.append(&spam_threshold_input);

    // mail-aliases-add-sheet-rate-per-hour-input (optional).
    let rate_per_hour_input = gtk::Entry::builder()
        .placeholder_text(S::RATE_PER_HOUR_PLACEHOLDER)
        .build();
    set_test_id(
        &rate_per_hour_input,
        ids::MAIL_ALIASES_ADD_SHEET_RATE_PER_HOUR_INPUT,
    );
    sheet_box.append(&rate_per_hour_input);

    // mail-aliases-add-sheet-ttl-input / -uses-input — disposable-only. The
    // create path doesn't mint disposables (those go via the generate button),
    // so they stay hidden in the seed; present in the tree so the page conforms
    // to ui.yaml's shared element set (same as mail-add-credential's conditional
    // fields).
    let ttl_input = gtk::Entry::builder()
        .placeholder_text(S::TTL_PLACEHOLDER)
        .visible(false)
        .build();
    set_test_id(&ttl_input, ids::MAIL_ALIASES_ADD_SHEET_TTL_INPUT);
    sheet_box.append(&ttl_input);
    let uses_input = gtk::Entry::builder()
        .placeholder_text(S::USES_PLACEHOLDER)
        .visible(false)
        .build();
    set_test_id(&uses_input, ids::MAIL_ALIASES_ADD_SHEET_USES_INPUT);
    sheet_box.append(&uses_input);
    // ttl_input/uses_input are now owned by the widget tree; the seed never
    // reads them back, so they're not threaded into AliasesWidgets.

    // mail-aliases-add-sheet-submit-button / -cancel-button.
    let submit_button = gtk::Button::builder()
        .label(S::SUBMIT)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(&submit_button, ids::MAIL_ALIASES_ADD_SHEET_SUBMIT_BUTTON);
    // `submit_sheet` dispatches `Create` in Add mode and `Update` in Edit mode
    // — a coin flip between two distinct kinds tui closed by carrying the mode
    // on its `MailAliasesSubmit` action.
    // This button is a persistent GTK widget, not rebuilt when `ctx.form_mode`
    // flips, so the shape here is `declare_wire_kind`'s re-declare: seeded
    // `Create` here (matching `FormMode::Add`, `wire_machine`'s seeded state),
    // then re-declared in `open_add_sheet`/`open_edit_sheet` — the two places
    // that ever change the mode — so the declaration never drifts from it.
    crate::offline_gate::declare_wire_kind(&submit_button, "fauna.bridges.create_account_alias");
    sheet_box.append(&submit_button);
    let cancel_button = gtk::Button::builder().label(S::CANCEL).build();
    set_test_id(&cancel_button, ids::MAIL_ALIASES_ADD_SHEET_CANCEL_BUTTON);
    sheet_box.append(&cancel_button);

    sheet_group.add(&sheet_box);
    page.add(&sheet_group);

    // --- Bulk paste-import inline sheet (hidden) ---
    // mail-aliases-import-sheet — the group *is* the sheet; the marker in its
    // header suffix carries the component id (same idiom as the
    // `mail-aliases-list` group marker) and is only mapped while the sheet is
    // revealed, so the e2e `is_visible` contract tracks the reveal.
    let import_sheet = adw::PreferencesGroup::builder()
        .title(S::IMPORT_TITLE)
        .description(S::IMPORT_SUBTITLE)
        .build();
    import_sheet.set_visible(false);
    import_sheet.set_header_suffix(Some(&super::marker("mail-aliases-import-sheet")));
    let import_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    import_box.set_margin_top(8);
    import_box.set_margin_bottom(8);

    // GtkTextView has no native placeholder (it's TextBuffer-backed, not a
    // gtk::Editable), so the "One address per line" hint the other apps pass
    // as the textarea's placeholder renders as a dim label above it.
    let import_hint = gtk::Label::builder()
        .label(S::IMPORT_PLACEHOLDER)
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    import_box.append(&import_hint);

    // mail-aliases-import-textarea — multi-line, one address per line. The
    // in-process automation agent drives a gtk::TextView through its TextBuffer
    // (`automation/agent.rs` type_text/clear; `automation/find.rs` text_of) —
    // the same widget `mail_list_members.rs`'s import sheet uses.
    let import_input = gtk::TextView::new();
    import_input.set_height_request(120);
    import_input.set_monospace(true);
    import_input.set_wrap_mode(gtk::WrapMode::WordChar);
    set_test_id(&import_input, ids::MAIL_ALIASES_IMPORT_TEXTAREA);
    let import_scroll = gtk::ScrolledWindow::builder().child(&import_input).build();
    import_box.append(&import_scroll);

    // mail-aliases-import-submit-button / -cancel-button.
    let import_submit = gtk::Button::builder()
        .label(S::IMPORT_SUBMIT)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(&import_submit, ids::MAIL_ALIASES_IMPORT_SUBMIT_BUTTON);
    crate::offline_gate::declare_wire_kind(&import_submit, "fauna.bridges.import_account_aliases");
    import_box.append(&import_submit);
    let import_cancel = gtk::Button::builder().label(S::IMPORT_CANCEL).build();
    set_test_id(&import_cancel, ids::MAIL_ALIASES_IMPORT_CANCEL_BUTTON);
    import_box.append(&import_cancel);

    // mail-aliases-import-result — the per-line outcome summary
    // ("N created · M already existed · K invalid") rendered from the snapshot's
    // `last_import_result`. A *readable* tagged label, not the 1px
    // `value_marker`: the user has to actually read the outcome (windows renders
    // the same string in a visible TextBlock — priority #1/#3). Wrapped +
    // width-capped so a long summary can't widen the embedded status view.
    let import_result = gtk::Label::builder()
        .label("")
        .visible(false)
        .wrap(true)
        .xalign(0.0)
        .max_width_chars(40)
        .css_classes(["dim-label"])
        .build();
    set_test_id(&import_result, ids::MAIL_ALIASES_IMPORT_RESULT);
    import_box.append(&import_result);

    import_sheet.add(&import_box);
    page.add(&import_sheet);

    let widgets = AliasesWidgets {
        error_label,
        add_button,
        import_button,
        generate_button,
        minted_toast,
        list_group,
        list_placeholder,
        rows: Rc::new(RefCell::new(Vec::new())),
        sheet_group,
        kind_picker,
        pattern_input,
        label_input,
        spam_threshold_input,
        rate_per_hour_input,
        submit_button,
        cancel_button,
        import_sheet,
        import_input,
        import_submit,
        import_cancel,
        import_result,
    };
    let refresh = wire_machine(widgets);

    (page, refresh)
}

/// Connect the page to the shared `MailAliasesMachine`, hydrate on mount, and
/// wire every interaction. Returns the shell's on-visible refresh closure —
/// a no-op (page stays at static placeholders) when no client is available,
/// e.g. the unit test, which has no registered client.
fn wire_machine(widgets: AliasesWidgets) -> Rc<dyn Fn()> {
    let client = match crate::settings::get_client() {
        Some(c) => c,
        // No client (unit test / pre-auth): the page stays at its placeholder,
        // so becoming visible later has nothing to refresh.
        None => return Rc::new(|| {}),
    };
    let machine = Arc::new(crate::mail_glue::build_mail_aliases_machine(&client));

    let ctx = Rc::new(AliasesCtx {
        machine,
        rt: client.runtime_handle(),
        form_mode: Cell::new(FormMode::Add),
        editing_id: RefCell::new(None),
        w: widgets,
    });

    hydrate_and_render(&ctx);

    // Add button → open the sheet in Add mode.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.add_button.clone().connect_clicked(move |_| {
            open_add_sheet(&ctx);
        });
    }

    // Generate-disposable button → mint with the per-user defaults.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.generate_button.clone().connect_clicked(move |_| {
            dispatch_action(
                &ctx,
                MailAliasesAction::GenerateDisposable {
                    ttl_days: None,
                    uses: None,
                    label: String::new(),
                },
            );
        });
    }

    // Submit → Create / Update.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.submit_button.clone().connect_clicked(move |_| {
            submit_sheet(&ctx);
        });
    }

    // Cancel → hide the sheet, re-render.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.cancel_button.clone().connect_clicked(move |_| {
            ctx.w.sheet_group.set_visible(false);
            hydrate_and_render(&ctx);
        });
    }

    // Import button → open the bulk paste-import sheet.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.import_button.clone().connect_clicked(move |_| {
            open_import_sheet(&ctx);
        });
    }

    // Import submit → Import { lines }.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.import_submit.clone().connect_clicked(move |_| {
            submit_import(&ctx);
        });
    }

    // Import cancel → hide the sheet, re-render.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.import_cancel.clone().connect_clicked(move |_| {
            ctx.w.import_sheet.set_visible(false);
            hydrate_and_render(&ctx);
        });
    }

    // The settings shell's on-visible hook (`views/settings_shell.rs`), which
    // eight sibling pages already use and which tui applies to this very page.
    // Without it this page DEADLOCKED: `render` gates add, generate AND import
    // on `snap.default_domain.is_some()`, and `default_domain`'s only writer is
    // the shared machine's `refresh` — reached from the list round trip those
    // very buttons perform. So a client that enabled mail after login (the page
    // having hydrated once here at settings-shell build time) had every control
    // that could fetch the domain disabled, with no way forward from the UI.
    // Measured 2026-08-28 at 60 s, and invisible for months because the e2e
    // action clicked the disabled button directly.
    Rc::new(move || hydrate_and_render(&ctx))
}

/// Open the add/edit sheet in `Add` mode, resetting it to an empty exact alias.
fn open_add_sheet(ctx: &Rc<AliasesCtx>) {
    ctx.form_mode.set(FormMode::Add);
    *ctx.editing_id.borrow_mut() = None;
    crate::offline_gate::declare_wire_kind(
        &ctx.w.submit_button,
        "fauna.bridges.create_account_alias",
    );
    let w = &ctx.w;
    // The add/edit and import sheets are mutually exclusive reveals.
    w.import_sheet.set_visible(false);
    w.kind_picker.set_active(false);
    w.kind_picker.set_sensitive(true);
    w.pattern_input.set_text("");
    w.label_input.set_text("");
    w.spam_threshold_input.set_text("");
    w.rate_per_hour_input.set_text("");
    w.submit_button.set_label(S::SUBMIT);
    super::render_error_label(&w.error_label, None);
    w.sheet_group.set_visible(true);
}

/// Open the sheet in `Edit` mode pre-populated from `view`. The kind picker is
/// read-only — kind is immutable (`mail-aliases.md`: can't change a wildcard
/// into a disposable mid-life).
fn open_edit_sheet(ctx: &Rc<AliasesCtx>, view: &AliasView) {
    ctx.form_mode.set(FormMode::Edit);
    *ctx.editing_id.borrow_mut() = Some(view.alias_id_hex.clone());
    crate::offline_gate::declare_wire_kind(
        &ctx.w.submit_button,
        "fauna.bridges.update_account_alias",
    );
    let w = &ctx.w;
    w.import_sheet.set_visible(false);
    w.kind_picker.set_active(view.kind == AliasKind::Wildcard);
    w.kind_picker.set_sensitive(false);
    w.pattern_input.set_text(&view.pattern);
    w.label_input.set_text(&view.label);
    w.spam_threshold_input.set_text(
        &view
            .spam_threshold_override
            .map(|v| v.to_string())
            .unwrap_or_default(),
    );
    w.rate_per_hour_input.set_text(
        &view
            .rate_limit_per_hour
            .map(|v| v.to_string())
            .unwrap_or_default(),
    );
    w.submit_button.set_label(S::SUBMIT);
    super::render_error_label(&w.error_label, None);
    w.sheet_group.set_visible(true);
}

/// Read the sheet, build the Create/Update action, dispatch, then render.
fn submit_sheet(ctx: &Rc<AliasesCtx>) {
    let w = &ctx.w;
    let pattern = w.pattern_input.text().trim().to_string();
    let label = w.label_input.text().trim().to_string();
    let spam_threshold_override = fauna_core::format::parse_count(&w.spam_threshold_input.text());
    let rate_limit_per_hour = fauna_core::format::parse_count_i64(&w.rate_per_hour_input.text());

    let action = match ctx.form_mode.get() {
        FormMode::Add => {
            let kind = if w.kind_picker.is_active() {
                AliasKind::Wildcard
            } else {
                AliasKind::Exact
            };
            MailAliasesAction::Create {
                kind,
                pattern,
                label,
                spam_threshold_override,
                rate_limit_per_hour,
            }
        }
        FormMode::Edit => {
            let Some(alias_id_hex) = ctx.editing_id.borrow().clone() else {
                return;
            };
            MailAliasesAction::Update {
                alias_id_hex,
                pattern,
                label,
                spam_threshold_override,
                rate_limit_per_hour,
            }
        }
    };

    let machine = Arc::clone(&ctx.machine);
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let _ = machine.dispatch(action).await;
            machine.snapshot()
        },
        move |snap| {
            render(&ctx_render, &snap);
            // Close the sheet only on success; on error render() surfaced
            // snap.error and we leave the sheet open for retry.
            if snap.error.is_none() {
                ctx_render.w.sheet_group.set_visible(false);
            }
        },
    );
}

/// Open the bulk paste-import sheet on an empty textarea + a cleared result
/// (`mail-aliases.md` § Bulk import).
fn open_import_sheet(ctx: &Rc<AliasesCtx>) {
    let w = &ctx.w;
    // Mutually exclusive with the add/edit sheet (mirrors mail_list_members.rs).
    w.sheet_group.set_visible(false);
    w.import_input.buffer().set_text("");
    w.import_result.set_text("");
    w.import_result.set_visible(false);
    super::render_error_label(&w.error_label, None);
    w.import_sheet.set_visible(true);
}

/// Read the import textarea, split it into per-line addresses, and dispatch
/// [`MailAliasesAction::Import`]; `render` then surfaces the per-line outcome
/// summary in `mail-aliases-import-result` (and the refreshed alias list — the
/// shared machine re-lists after a successful import, `aliases.rs::import`).
///
/// Each line is trimmed and blanks are dropped **client-side**, per
/// `mail-aliases.md` § Bulk import ("Blank lines are trimmed and produce no
/// outcome") — a trailing newline in a pasted block must not become an `invalid`
/// outcome. Everything else (validation, dedupe, per-line status) is the nest's
/// + the shared machine's job; this layer holds no import logic.
///
/// The sheet deliberately stays open on submit: `mail-aliases-import-result`
/// lives *inside* it (ui.yaml's `mail-aliases-import-sheet` component), so
/// closing it on success would hide the very outcome the user asked for. Cancel
/// closes it.
fn submit_import(ctx: &Rc<AliasesCtx>) {
    let buf = ctx.w.import_input.buffer();
    let (start, end) = buf.bounds();
    let text = buf.text(&start, &end, false).to_string();
    let lines: Vec<String> = text
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();
    if lines.is_empty() {
        return;
    }
    dispatch_action(ctx, MailAliasesAction::Import { lines });
}

/// Run `machine.hydrate()` on the tokio runtime (retrying while the WS socket
/// comes up after login), then render the snapshot on the GTK main thread.
fn hydrate_and_render(ctx: &Rc<AliasesCtx>) {
    let machine = Arc::clone(&ctx.machine);
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let _ = machine.hydrate().await;
            machine.snapshot()
        },
        move |snap| render(&ctx_render, &snap),
    );
}

/// Dispatch a fire-and-render action (generate / revoke / delete) on the tokio
/// runtime, then render the resulting snapshot.
fn dispatch_action(ctx: &Rc<AliasesCtx>, action: MailAliasesAction) {
    let machine = Arc::clone(&ctx.machine);
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let _ = machine.dispatch(action).await;
            machine.snapshot()
        },
        move |snap| render(&ctx_render, &snap),
    );
}

/// Render a `MailAliasesSnapshot` into the page widgets (GTK main thread).
fn render(ctx: &Rc<AliasesCtx>, snap: &MailAliasesSnapshot) {
    let w = &ctx.w;

    super::render_error_label(&w.error_label, snap.error.as_deref());

    // Create/generate require a canonical exact alias (default_domain); without
    // one the nest would reject, so disable the add controls + say why.
    let has_domain = snap.default_domain.is_some();
    w.add_button.set_sensitive(has_domain);
    w.generate_button.set_sensitive(has_domain);
    // Import creates exact aliases too, so it needs mail enabled just like
    // add/generate (same gate as windows' `ImportButton.IsEnabled = CanManage`).
    w.import_button.set_sensitive(has_domain);
    if !has_domain && snap.error.is_none() {
        super::render_error_label(&w.error_label, Some(S::NO_DEFAULT_DOMAIN));
    }

    // Disposable-mint success: copy the full address + show a transient toast.
    if let Some(addr) = &snap.last_minted_address {
        crate::clipboard::copy_text(addr);
        w.minted_toast.set_text(&format!("{} {addr}", S::COPIED));
        w.minted_toast.set_visible(true);
        let toast = w.minted_toast.downgrade();
        glib::timeout_add_local_once(Duration::from_secs(4), move || {
            if let Some(t) = toast.upgrade() {
                t.set_visible(false);
            }
        });
    }

    // Bulk-import outcome (mail-aliases-import-result): the shared
    // `ImportResultView` counts rendered through the `mail_aliases.import_result`
    // template ("{created} created · {existed} already existed · {invalid}
    // invalid" — `existed` is the view's `skipped_duplicate`), plus one
    // `mail_aliases.import_invalid_line` row per invalid outcome
    // (mail-aliases.md:199-201 requires the reason be rendered, not just
    // tallied). The machine clears `last_import_result` at the start of the
    // next dispatch, so the label hides itself again on any following action.
    match &snap.last_import_result {
        Some(result) => {
            let mut text = S::import_result(
                &result.created.to_string(),
                &result.skipped_duplicate.to_string(),
                &result.invalid.to_string(),
            );
            for outcome in &result.outcomes {
                if outcome.status == ImportAliasStatusView::Invalid {
                    text.push('\n');
                    text.push_str(&S::import_invalid_line(
                        &outcome.address,
                        outcome.reason.as_deref().unwrap_or(""),
                    ));
                }
            }
            w.import_result.set_text(&text);
            w.import_result.set_visible(true);
        }
        None => {
            w.import_result.set_text("");
            w.import_result.set_visible(false);
        }
    }

    // Aliases list: tear down the previous rows, rebuild from the snapshot.
    {
        let mut rows = w.rows.borrow_mut();
        for row in rows.drain(..) {
            w.list_group.remove(&row);
        }
        for view in &snap.aliases {
            let row = build_alias_row(ctx, view);
            w.list_group.add(&row);
            rows.push(row);
        }
    }
    // A real snapshot has landed — the loading window is over, so the
    // placeholder (if shown at all) now means "genuinely empty," not
    // "unknown yet."
    w.list_placeholder.set_title(S::EMPTY);
    w.list_placeholder.set_visible(snap.aliases.is_empty());
}

/// Build one `mail-aliases-list-item` row from an [`AliasView`]. The row
/// itself carries the `mail-aliases-list-item` id (not a sibling marker — a
/// scoped lookup like `mail-aliases-list-item[i]/mail-aliases-list-item-
/// revoke-button` walks the *named widget's own descendants*
/// (`automation/find.rs::scope_root`/`collect_in`), and prefix/suffix
/// children of an `adw::ActionRow` are only reachable that way if the row
/// itself carries the id, mirroring `labeler-catalog-item`'s row-is-the-
/// container shape). Read-only fields carry 1px marker labels; the revoke +
/// overflow-delete are two-click inline confirms (no modal).
fn build_alias_row(ctx: &Rc<AliasesCtx>, view: &AliasView) -> adw::ActionRow {
    let kind_label = fauna_client_mail_settings::alias_kind_badge(view.kind)
        .resolve(crate::i18n::strings::lookup);
    let row = adw::ActionRow::builder()
        .title(&view.address)
        .subtitle(if view.label.is_empty() {
            kind_label.to_string()
        } else {
            format!("{kind_label} · {}", view.label)
        })
        .build();
    set_test_id(&row, ids::MAIL_ALIASES_LIST_ITEM);
    // The row's identity is the address (title); the subtitle is kind/label
    // detail. Declare it — see `testid::set_test_text` for the full story.
    crate::testid::set_test_text(&row, &view.address);

    // Read-only marker labels carrying per-field text for the e2e driver.
    row.add_suffix(&super::value_marker(
        "mail-aliases-list-item-pattern",
        &view.address,
    ));
    row.add_suffix(&super::value_marker(
        "mail-aliases-list-item-kind",
        &kind_label,
    ));
    row.add_suffix(&super::value_marker(
        "mail-aliases-list-item-label",
        &view.label,
    ));
    row.add_suffix(&super::value_marker(
        "mail-aliases-list-item-hits",
        &format_hits(view),
    ));

    // The canonical `<handle>@<domain>` row is the user's primary mailbox +
    // AUTH-login identity; the nest rejects disabling, renaming, or deleting it
    // (`canonical_alias_protected`). Render it **read-only** — no
    // toggle/edit/revoke/delete, marked as the primary address — so the
    // protection is visible rather than surfacing only as an error on attempt
    // (`mail-aliases.md` § Aliases UX). This closes the live-found disable-trap
    // at the UI layer; the nest guard backs it for every app. The inert
    // audit disclosure is kept for parity (mail history of the primary address).
    if view.is_canonical {
        let primary_badge = gtk::Label::new(Some(S::PRIMARY_ADDRESS_BADGE));
        primary_badge.add_css_class("dim-label");
        primary_badge.set_valign(gtk::Align::Center);
        primary_badge.set_tooltip_text(Some(S::PRIMARY_ADDRESS_TOOLTIP));
        row.add_suffix(&primary_badge);
        row.add_suffix(&build_show_audit_button());
        return row;
    }

    // mail-aliases-list-item-disabled-toggle — the alias on/off switch, now
    // **two-way** and **labelled** ("Active"): ON = receiving (enabled); turning
    // it OFF disables (→ Revoke), turning it back ON re-enables (→ Enable). The
    // disable direction is no longer a one-way trap (`mail-aliases.md:156`; the
    // nest also rejects disabling the canonical address, surfaced as an error).
    let active_label = gtk::Label::new(Some(S::ACTIVE_TOGGLE_LABEL));
    active_label.add_css_class("dim-label");
    let disabled_toggle = gtk::Switch::builder()
        .active(!view.disabled)
        .valign(gtk::Align::Center)
        .tooltip_text(S::ACTIVE_TOGGLE_TOOLTIP)
        .build();
    set_test_id(
        &disabled_toggle,
        ids::MAIL_ALIASES_LIST_ITEM_DISABLED_TOGGLE,
    );
    // ON dispatches `Enable` (`fauna.bridges.enable_account_alias`); OFF
    // dispatches `Revoke` (`fauna.bridges.revoke_account_alias`) — distinct
    // kinds on one control, the case tui closed on its
    // `MailAliasesToggleActive` action.
    // Unlike the submit button above, this widget is torn down and rebuilt on
    // every list refresh (`hydrate_and_render`'s `list_group.remove` +
    // `build_alias_row` pass), so `view.disabled` is read fresh at
    // construction every time — no re-declare call is needed, only the one
    // declaration here, keyed on what the NEXT click (the flip from today's
    // state) will issue.
    crate::offline_gate::declare_wire_kind(
        &disabled_toggle,
        if view.disabled {
            "fauna.bridges.enable_account_alias"
        } else {
            "fauna.bridges.revoke_account_alias"
        },
    );
    {
        let ctx = Rc::clone(ctx);
        let alias_id_hex = view.alias_id_hex.clone();
        disabled_toggle.connect_active_notify(move |sw| {
            // ON → Enable (re-enable); OFF → Revoke (soft-off). On rebuild the
            // switch is constructed with its state before this handler is
            // connected, so a snapshot refresh never re-fires it.
            let action = if sw.is_active() {
                MailAliasesAction::Enable {
                    alias_id_hex: alias_id_hex.clone(),
                }
            } else {
                MailAliasesAction::Revoke {
                    alias_id_hex: alias_id_hex.clone(),
                }
            };
            dispatch_action(&ctx, action);
        });
    }
    let toggle_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    toggle_box.set_valign(gtk::Align::Center);
    toggle_box.append(&active_label);
    toggle_box.append(&disabled_toggle);
    row.add_suffix(&toggle_box);

    // mail-aliases-list-item-edit-button — opens the sheet pre-populated.
    let edit_button = gtk::Button::builder()
        .label(S::EDIT)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    set_test_id(&edit_button, ids::MAIL_ALIASES_LIST_ITEM_EDIT_BUTTON);
    {
        let ctx = Rc::clone(ctx);
        let view = view.clone();
        edit_button.connect_clicked(move |_| open_edit_sheet(&ctx, &view));
    }
    row.add_suffix(&edit_button);

    // mail-aliases-list-item-revoke-button — two-click inline confirm → Revoke.
    let revoke_button = gtk::Button::builder()
        .label(S::REVOKE)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    set_test_id(&revoke_button, ids::MAIL_ALIASES_LIST_ITEM_REVOKE_BUTTON);
    crate::offline_gate::declare_wire_kind(&revoke_button, "fauna.bridges.revoke_account_alias");
    {
        let ctx = Rc::clone(ctx);
        let alias_id_hex = view.alias_id_hex.clone();
        super::wire_two_click(
            &revoke_button,
            S::REVOKE,
            crate::i18n::strings::common::CONFIRM_Q,
            false,
            |btn| {
                btn.remove_css_class("flat");
                btn.add_css_class("destructive-action");
            },
            |btn| {
                btn.remove_css_class("destructive-action");
                btn.add_css_class("flat");
            },
            move || {
                dispatch_action(
                    &ctx,
                    MailAliasesAction::Revoke {
                        alias_id_hex: alias_id_hex.clone(),
                    },
                )
            },
        );
    }
    row.add_suffix(&revoke_button);

    // mail-aliases-list-item-overflow-menu — overflow Delete (destructive,
    // irreversible). Two-click inline confirm, no modal.
    let overflow_menu = gtk::Button::builder()
        .label(S::DELETE)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    set_test_id(&overflow_menu, ids::MAIL_ALIASES_LIST_ITEM_OVERFLOW_MENU);
    crate::offline_gate::declare_wire_kind(&overflow_menu, "fauna.bridges.delete_account_alias");
    {
        let ctx = Rc::clone(ctx);
        let alias_id_hex = view.alias_id_hex.clone();
        super::wire_two_click(
            &overflow_menu,
            S::DELETE,
            crate::i18n::strings::common::CONFIRM_Q,
            false,
            |btn| {
                btn.remove_css_class("flat");
                btn.add_css_class("destructive-action");
            },
            |btn| {
                btn.remove_css_class("destructive-action");
                btn.add_css_class("flat");
            },
            move || {
                dispatch_action(
                    &ctx,
                    MailAliasesAction::Delete {
                        alias_id_hex: alias_id_hex.clone(),
                    },
                )
            },
        );
    }
    row.add_suffix(&overflow_menu);

    row.add_suffix(&build_show_audit_button());

    row
}

/// `mail-aliases-list-item-show-audit` — inert disclosure (audit listing is a
/// follow-on slice; the machine doesn't expose `list_account_alias_hits` yet).
/// Shared by the ordinary and canonical (read-only) row layouts.
fn build_show_audit_button() -> gtk::Button {
    let show_audit = gtk::Button::builder()
        .label(S::SHOW_AUDIT)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .sensitive(false)
        .build();
    set_test_id(&show_audit, ids::MAIL_ALIASES_LIST_ITEM_SHOW_AUDIT);
    show_audit
}

/// `mail-aliases-list-item-hits` text: hit count + last-hit date if any. The
/// surrounding English template is single-sourced in the shared
/// `fauna_client_mail_settings::alias_hits_label`; only the native local-tz date
/// formatting (`crate::i18n::local_date`) stays here.
fn format_hits(view: &AliasView) -> String {
    fauna_client_mail_settings::alias_hits_label(
        view.hit_count,
        view.last_hit_at_ms.map(crate::i18n::local_date),
    )
    .resolve(crate::i18n::strings::lookup)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testid::widget_names;

    /// The Aliases page exposes every static ui.yaml ID for the `mail-aliases`
    /// page + the add-sheet fields. The indexed `mail-aliases-list-item*` rows
    /// are added from the snapshot at render time (no registered client in this
    /// test), so they're not asserted here.
    #[test]
    fn aliases_page_exposes_static_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let (page, _refresh) = build_mail_aliases_page();
            let names = widget_names(&page);
            for id in [
                "page-heading",
                "error-message",
                "mail-aliases-add-button",
                "mail-aliases-import-button",
                "mail-aliases-generate-disposable-button",
                "mail-aliases-list",
                "mail-aliases-add-sheet-kind-picker",
                "mail-aliases-add-sheet-pattern-input",
                "mail-aliases-add-sheet-label-input",
                "mail-aliases-add-sheet-spam-threshold-input",
                "mail-aliases-add-sheet-rate-per-hour-input",
                "mail-aliases-add-sheet-ttl-input",
                "mail-aliases-add-sheet-uses-input",
                "mail-aliases-add-sheet-submit-button",
                "mail-aliases-add-sheet-cancel-button",
                // The bulk paste-import sheet (ui.yaml `mail-aliases-import-sheet`
                // component). Hidden until `mail-aliases-import-button` reveals it,
                // but present in the widget tree from page-build.
                "mail-aliases-import-sheet",
                "mail-aliases-import-textarea",
                "mail-aliases-import-submit-button",
                "mail-aliases-import-cancel-button",
                "mail-aliases-import-result",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}",
                );
            }
        });
    }
}
