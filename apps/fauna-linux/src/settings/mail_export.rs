//! The user-facing "Export mailbox" wizard page (linux; the mail-UX seed's lead
//! app).
//!
//! A five-step wizard (Format → Scope → Confirm → Progress → Done) that pulls the
//! user's mail-area state out in one of three MUA-portable formats, sealed-blob
//! delivered. Target behavior: `docs/goal/behavior/mail-export.md` § UX shape /
//! § Session row model / § Wire shapes. UX/IDs: `tests/e2e-unified/ui.yaml`
//! `mail-export` page + `mail-export-mailbox-progress-list` component.
//!
//! Per `mail-export.md` § Where logic lives this layer holds **no** business
//! logic — it is a dumb renderer of [`MailExportSnapshot`] + dispatcher of
//! [`MailExportAction`]; the wizard FSM (step transitions, format pick, scope
//! default-selection, the start/pause/resume/cancel sequencing) lives in the
//! shared `fauna_client_mail_settings::export` machine (priority #2/#4), the prior
//! art the other five apps lift. Direct sibling: `settings/mail_spam.rs`.
//!
//! # One element set, shown conditionally by step
//!
//! All five steps' `mail-export-*` widgets are built once into the tree (so every
//! ui.yaml ID is statically present); `render()` shows only the active step's
//! group (same pattern the goal doc names — `mail-add-credential`'s conditional
//! fields). The Format→Scope→Confirm navigation is client-side, on the SHARED
//! `wizard-next-button` / `wizard-back-button` ids (real ui.yaml elements here
//! since the 2026-08-29 approval that mirrored `mail-import`'s already-ratified
//! shape — they were untagged before); the durable commit is the tagged
//! `mail-export-start-button`.
//!
//! Each shared nav id is painted TWICE (Format+Scope each carry a Next,
//! Scope+Confirm each carry a Back) and is disambiguated purely by the step
//! gating above: `automation::find` prunes non-showing subtrees, so exactly one
//! of each is findable at a time — the same mechanism that keeps a background
//! stack page's `error-message` from shadowing the live one. That is why the
//! non-Format groups are built `visible(false)` rather than merely hidden on
//! the first render: an ambiguous window between build and first hydrate would
//! be a real flake. `settings/mail_import.rs` is the twin, built the same way.
//!
//! # This page drives the export (`mail-export.md` § Implementation status today)
//!
//! The machine has key custody (`mail_glue::build_mail_export_machine`), so the
//! page does the three things custody obliges — tui's leg, lifted:
//!
//! 1. **It spawns `MailExportMachine::run_export`** after a `Start` or `Resume`
//!    whose post-dispatch snapshot reads `Running` (`after_snapshot`). Custody
//!    and the spawn land together: custody alone would open a session nothing
//!    drives — a Progress screen stuck at zero holding one of the user's three
//!    concurrency slots.
//! 2. **It repaints Progress on a tick** while the loop mutates the machine's
//!    snapshot (`start_progress_tick`, `mail_import.rs`'s twin). The tick only
//!    renders; Cancel's two-click arm is widget state (`wire_two_click`), which
//!    `render()` never touches, so a repaint cannot disarm a Cancel the user just
//!    armed — the trap tui's `disarm_cancel` exists for.
//! 3. **Download runs § Download flow** (`MailExportAction::Download`): the
//!    archive lands in the user's downloads directory and the Done summary says
//!    where (`mail_export.saved_summary_fmt`), so the press is visible; a refused
//!    archive surfaces on `error-message` and leaves no file behind.

use fauna_ui_ids as ids;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

use fauna_client_mail_settings::{
    ExportFormat, ExportSessionState, ExportStep, MailExportAction, MailExportMachine,
    MailExportSnapshot,
};

use crate::async_helper::spawn_with_snapshot;
use crate::i18n::strings::mail_export as S;
use crate::testid::set_test_id;

/// Handles to the widgets the snapshot renders into.
#[derive(Clone)]
struct ExportWidgets {
    error_label: gtk::Label,

    // Step 1 — Format.
    format_group: adw::PreferencesGroup,
    format_picker: gtk::DropDown,

    // Step 2 — Scope.
    scope_group: adw::PreferencesGroup,
    mailboxes_group: adw::PreferencesGroup,
    mailbox_rows: Rc<std::cell::RefCell<Vec<gtk::CheckButton>>>,
    mailboxes_placeholder: adw::ActionRow,
    date_from_input: gtk::Entry,
    date_to_input: gtk::Entry,
    strip_headers_toggle: gtk::Switch,

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
    download_url: gtk::Label,
}

/// Everything the page's handlers + render need. `Rc`-shared into every closure.
struct ExportCtx {
    machine: Arc<MailExportMachine>,
    rt: tokio::runtime::Handle,
    /// Set while `render()` programmatically updates the format picker, so its
    /// `selected_notify` handler doesn't echo the change back as a dispatch.
    syncing: Cell<bool>,
    /// Set while the Progress repaint tick is scheduled, so a second
    /// `Start`/`Resume` never stacks a second timer.
    ticking: Cell<bool>,
    w: ExportWidgets,
}

/// A tagged wizard nav button (Next / Back). These carry the SHARED
/// `wizard-next-button` / `wizard-back-button` ids — real ui.yaml elements on
/// this page since the 2026-08-29 approval that mirrored `mail-import`'s
/// already-ratified shape. Each id is painted twice (Format+Scope carry a Next,
/// Scope+Confirm carry a Back) and is disambiguated purely by step gating:
/// `automation::find` prunes non-showing subtrees, so exactly one of each is
/// findable at a time. That is why the non-Format groups are built
/// `visible(false)` rather than merely hidden on the first render.
fn nav_button(id: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .label(label)
        .valign(gtk::Align::Center)
        .build();
    set_test_id(&button, id);
    button
}

/// Build the "Export mailbox" wizard page.
pub fn build_mail_export_page() -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title(S::TITLE)
        .icon_name("document-save-symbolic")
        .build();

    // --- Heading + page-level error (always visible) ---
    let top_group = adw::PreferencesGroup::builder()
        .title(S::TITLE)
        .description(S::DESCRIPTION)
        .build();
    top_group.set_header_suffix(Some(&super::marker("page-heading")));
    let error_label = gtk::Label::builder().visible(false).build();
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    let error_row = adw::ActionRow::builder().activatable(false).build();
    error_row.add_suffix(&error_label);
    top_group.add(&error_row);
    page.add(&top_group);

    // === Step 1 — Format =====================================================
    let format_group = adw::PreferencesGroup::builder()
        .title(S::FORMAT_TITLE)
        .build();
    // mail-export-format-picker — a 3-way DropDown (the in-process agent reads it
    // via driver.select). value-via-state, ID on the DropDown.
    let format_picker =
        gtk::DropDown::from_strings(&[S::FORMAT_MBOX, S::FORMAT_MAILDIR, S::FORMAT_EML]);
    format_picker.set_valign(gtk::Align::Center);
    set_test_id(&format_picker, ids::MAIL_EXPORT_FORMAT_PICKER);
    let format_row = adw::ActionRow::builder().title(S::FORMAT_TITLE).build();
    format_row.add_suffix(&format_picker);
    format_group.add(&format_row);
    let format_next = nav_button(ids::WIZARD_NEXT_BUTTON, S::NEXT);
    let format_next_row = adw::ActionRow::builder().activatable(false).build();
    format_next_row.add_suffix(&format_next);
    format_group.add(&format_next_row);
    page.add(&format_group);

    // === Step 2 — Scope ======================================================
    let scope_group = adw::PreferencesGroup::builder()
        .title(S::SCOPE_TITLE)
        .visible(false)
        .build();
    // mail-export-scope-mailboxes — the multi-select container (per-mailbox
    // CheckButtons are rebuilt from the snapshot; the ID rides the group marker).
    let mailboxes_group = adw::PreferencesGroup::builder()
        .title(S::SCOPE_MAILBOXES_LABEL)
        .visible(false)
        .build();
    mailboxes_group.set_header_suffix(Some(&super::marker("mail-export-scope-mailboxes")));
    let mailboxes_placeholder = adw::ActionRow::builder()
        .title(S::SCOPE_MAILBOXES_EMPTY)
        .build();
    mailboxes_group.add(&mailboxes_placeholder);

    let date_from_input = gtk::Entry::builder()
        .placeholder_text(S::SCOPE_DATE_FROM_PLACEHOLDER)
        .build();
    set_test_id(&date_from_input, ids::MAIL_EXPORT_SCOPE_DATE_FROM);
    let date_from_row = adw::ActionRow::builder().activatable(false).build();
    date_from_row.add_suffix(&date_from_input);

    let date_to_input = gtk::Entry::builder()
        .placeholder_text(S::SCOPE_DATE_TO_PLACEHOLDER)
        .build();
    set_test_id(&date_to_input, ids::MAIL_EXPORT_SCOPE_DATE_TO);
    let date_to_row = adw::ActionRow::builder().activatable(false).build();
    date_to_row.add_suffix(&date_to_input);

    let strip_headers_toggle = gtk::Switch::builder().valign(gtk::Align::Center).build();
    set_test_id(
        &strip_headers_toggle,
        ids::MAIL_EXPORT_SCOPE_STRIP_HEADERS_TOGGLE,
    );
    let strip_row = adw::ActionRow::builder()
        .title(S::SCOPE_STRIP_HEADERS_LABEL)
        .subtitle(S::SCOPE_STRIP_HEADERS_SUBTITLE)
        .activatable(false)
        .build();
    strip_row.add_suffix(&strip_headers_toggle);

    scope_group.add(&date_from_row);
    scope_group.add(&date_to_row);
    scope_group.add(&strip_row);

    let scope_back = nav_button(ids::WIZARD_BACK_BUTTON, S::BACK);
    let scope_next = nav_button(ids::WIZARD_NEXT_BUTTON, S::NEXT);
    let scope_nav_row = adw::ActionRow::builder().activatable(false).build();
    scope_nav_row.add_suffix(&scope_back);
    scope_nav_row.add_suffix(&scope_next);
    scope_group.add(&scope_nav_row);

    // Step 2 is two top-level groups (both gated visible at the Scope step): the
    // mailbox multi-select, then the scope fields.
    page.add(&mailboxes_group);
    page.add(&scope_group);

    // === Step 3 — Confirm ====================================================
    let confirm_group = adw::PreferencesGroup::builder()
        .title(S::CONFIRM_TITLE)
        .visible(false)
        .build();
    let confirm_summary = super::blank_value_marker("mail-export-confirm-summary");
    let confirm_row = adw::ActionRow::builder()
        .title(S::CONFIRM_TITLE)
        .subtitle(S::CONFIRM_PENDING)
        .build();
    confirm_row.add_suffix(&confirm_summary);
    confirm_group.add(&confirm_row);
    let start_button = gtk::Button::builder()
        .label(S::START_BUTTON)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(&start_button, ids::MAIL_EXPORT_START_BUTTON);
    let confirm_back = nav_button(ids::WIZARD_BACK_BUTTON, S::BACK);
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
    let progress_summary = super::blank_value_marker("mail-export-progress-summary");
    let progress_summary_row = adw::ActionRow::builder().title(S::PROGRESS_TITLE).build();
    progress_summary_row.add_suffix(&progress_summary);
    progress_group.add(&progress_summary_row);
    let progress_bar = gtk::ProgressBar::builder()
        .valign(gtk::Align::Center)
        .hexpand(true)
        .build();
    set_test_id(&progress_bar, ids::MAIL_EXPORT_PROGRESS_BAR);
    progress_group.add(&progress_bar);

    let pause_button = gtk::Button::builder()
        .label(S::PAUSE_BUTTON)
        .valign(gtk::Align::Center)
        .build();
    set_test_id(&pause_button, ids::MAIL_EXPORT_PAUSE_BUTTON);
    let resume_button = gtk::Button::builder()
        .label(S::RESUME_BUTTON)
        .valign(gtk::Align::Center)
        .build();
    set_test_id(&resume_button, ids::MAIL_EXPORT_RESUME_BUTTON);
    let cancel_button = gtk::Button::builder()
        .label(S::CANCEL_BUTTON)
        .valign(gtk::Align::Center)
        .css_classes(["destructive-action"])
        .build();
    set_test_id(&cancel_button, ids::MAIL_EXPORT_CANCEL_BUTTON);
    let progress_controls_row = adw::ActionRow::builder().activatable(false).build();
    progress_controls_row.add_suffix(&pause_button);
    progress_controls_row.add_suffix(&resume_button);
    progress_controls_row.add_suffix(&cancel_button);
    progress_group.add(&progress_controls_row);

    let error_log = super::blank_value_marker("mail-export-error-log");
    let error_log_row = adw::ActionRow::builder().title(S::ERROR_LOG_TITLE).build();
    error_log_row.add_suffix(&error_log);
    progress_group.add(&error_log_row);

    let mailbox_progress_group = adw::PreferencesGroup::new();
    mailbox_progress_group
        .set_header_suffix(Some(&super::marker("mail-export-mailbox-progress-list")));
    progress_group.add(&mailbox_progress_group);
    page.add(&progress_group);

    // === Step 5 — Done =======================================================
    let done_group = adw::PreferencesGroup::builder()
        .title(S::DONE_TITLE)
        .visible(false)
        .build();
    let done_summary = super::blank_value_marker("mail-export-done-summary");
    let done_summary_row = adw::ActionRow::builder().title(S::DONE_TITLE).build();
    done_summary_row.add_suffix(&done_summary);
    done_group.add(&done_summary_row);
    let download_button = gtk::Button::builder()
        .label(S::DOWNLOAD_BUTTON)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    set_test_id(&download_button, ids::MAIL_EXPORT_DOWNLOAD_BUTTON);
    let download_row = adw::ActionRow::builder().activatable(false).build();
    download_row.add_suffix(&download_button);
    done_group.add(&download_row);
    let download_url = super::blank_value_marker("mail-export-download-url");
    let download_url_row = adw::ActionRow::builder()
        .title(S::DOWNLOAD_URL_LABEL)
        .build();
    download_url_row.add_suffix(&download_url);
    done_group.add(&download_url_row);
    let discard_button = gtk::Button::builder()
        .label(S::DISCARD_BUTTON)
        .valign(gtk::Align::Center)
        .build();
    set_test_id(&discard_button, ids::MAIL_EXPORT_DISCARD_BUTTON);
    let discard_row = adw::ActionRow::builder().activatable(false).build();
    discard_row.add_suffix(&discard_button);
    done_group.add(&discard_row);
    page.add(&done_group);

    let widgets = ExportWidgets {
        error_label,
        format_group,
        format_picker,
        scope_group,
        mailboxes_group,
        mailbox_rows: Rc::new(std::cell::RefCell::new(Vec::new())),
        mailboxes_placeholder,
        date_from_input,
        date_to_input,
        strip_headers_toggle,
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
        download_url,
    };
    // Re-read the export sessions every time the page is shown — tui's
    // self-hydrate-on-every-visit, and for its reasons: an export started,
    // finished, discarded or cancelled on another of the user's devices changes
    // what this page must show, and the settings shell builds every sub-page
    // once at app init, so without this the wizard would keep its build-time
    // hydrate for the whole run of the app (`mail_spam.rs`'s `connect_map`).
    if let Some(refresh) = wire_machine(
        widgets,
        WizardButtons {
            format_next,
            scope_back,
            scope_next,
            confirm_back,
            start_button,
            pause_button,
            resume_button,
            cancel_button,
            download_button,
            discard_button,
        },
    ) {
        page.connect_map(move |_| refresh());
    }

    page
}

/// The interaction buttons threaded into `wire_machine` (kept out of
/// `ExportWidgets`, which only holds widgets `render()` writes into).
struct WizardButtons {
    format_next: gtk::Button,
    scope_back: gtk::Button,
    scope_next: gtk::Button,
    confirm_back: gtk::Button,
    start_button: gtk::Button,
    pause_button: gtk::Button,
    resume_button: gtk::Button,
    cancel_button: gtk::Button,
    download_button: gtk::Button,
    discard_button: gtk::Button,
}

/// Connect the page to the shared `MailExportMachine`, hydrate on mount, and wire
/// every interaction. Returns the re-hydrate the page runs each time it is
/// shown; `None` (and a no-op) when no client is available (the unit test).
fn wire_machine(widgets: ExportWidgets, b: WizardButtons) -> Option<impl Fn() + 'static> {
    let client = crate::settings::get_client()?;
    let machine = Arc::new(crate::mail_glue::build_mail_export_machine(&client));

    let ctx = Rc::new(ExportCtx {
        machine,
        rt: client.runtime_handle(),
        syncing: Cell::new(false),
        ticking: Cell::new(false),
        w: widgets,
    });

    hydrate_and_render(&ctx);

    // Format picker → SelectFormat (skip render()'s echo).
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .format_picker
            .clone()
            .connect_selected_notify(move |dd| {
                if ctx.syncing.get() {
                    return;
                }
                dispatch_action(
                    &ctx,
                    MailExportAction::SelectFormat {
                        format: format_at(dd.selected()),
                    },
                );
            });
    }
    // Scope inputs.
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.date_from_input.clone().connect_changed(move |e| {
            dispatch_action(
                &ctx,
                MailExportAction::SetDateFrom {
                    value: e.text().trim().to_string(),
                },
            );
        });
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w.date_to_input.clone().connect_changed(move |e| {
            dispatch_action(
                &ctx,
                MailExportAction::SetDateTo {
                    value: e.text().trim().to_string(),
                },
            );
        });
    }
    {
        let ctx = Rc::clone(&ctx);
        ctx.w
            .strip_headers_toggle
            .clone()
            .connect_active_notify(move |sw| {
                if ctx.syncing.get() {
                    return;
                }
                dispatch_action(
                    &ctx,
                    MailExportAction::SetStripHeaders { on: sw.is_active() },
                );
            });
    }

    // Navigation + lifecycle buttons.
    wire_dispatch(&ctx, &b.format_next, MailExportAction::Next);
    wire_dispatch(&ctx, &b.scope_back, MailExportAction::Back);
    wire_dispatch(&ctx, &b.scope_next, MailExportAction::Next);
    wire_dispatch(&ctx, &b.confirm_back, MailExportAction::Back);
    wire_dispatch(&ctx, &b.start_button, MailExportAction::Start);
    wire_dispatch(&ctx, &b.pause_button, MailExportAction::Pause);
    wire_dispatch(&ctx, &b.resume_button, MailExportAction::Resume);
    wire_dispatch(&ctx, &b.download_button, MailExportAction::Download);
    wire_dispatch(&ctx, &b.discard_button, MailExportAction::Discard);
    // Cancel is destructive — two-click inline confirm.
    let refresh_ctx = Rc::clone(&ctx);
    super::wire_two_click(
        &b.cancel_button,
        S::CANCEL_BUTTON,
        crate::i18n::strings::common::CONFIRM_Q,
        true,
        |_| {},
        |_| {},
        move || dispatch_action(&ctx, MailExportAction::Cancel),
    );
    Some(move || hydrate_and_render(&refresh_ctx))
}

/// Wire a button to a single fire-and-render action.
fn wire_dispatch(ctx: &Rc<ExportCtx>, button: &gtk::Button, action: MailExportAction) {
    let ctx = Rc::clone(ctx);
    button.connect_clicked(move |_| dispatch_action(&ctx, action.clone()));
}

/// `ExportFormat` for a DropDown position (0 = mbox, 1 = maildir, 2 = eml-zip).
fn format_at(index: u32) -> ExportFormat {
    match index {
        1 => ExportFormat::MaildirPlus,
        2 => ExportFormat::EmlZip,
        _ => ExportFormat::Mbox,
    }
}

/// DropDown position for an `ExportFormat`.
fn index_of(format: ExportFormat) -> u32 {
    match format {
        ExportFormat::Mbox => 0,
        ExportFormat::MaildirPlus => 1,
        ExportFormat::EmlZip => 2,
    }
}

fn hydrate_and_render(ctx: &Rc<ExportCtx>) {
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

fn dispatch_action(ctx: &Rc<ExportCtx>, action: MailExportAction) {
    let should_run = matches!(action, MailExportAction::Start | MailExportAction::Resume);
    if should_run || matches!(action, MailExportAction::Download) {
        // The handle names the archive's root directory and the saved file, and
        // arrives after sign-in (`AccountLoaded`) or changes with the user — so
        // it is read here, at the gesture, never baked at page build.
        if let Some(handle) = crate::settings::get_handle() {
            ctx.machine.set_actor_handle(handle);
        }
    }
    let machine = Arc::clone(&ctx.machine);
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            let _ = machine.dispatch(action).await;
            machine.snapshot()
        },
        move |snap| after_snapshot(&ctx_render, &snap, should_run),
    );
}

/// Render `snap`, and — when it followed a `Start`/`Resume` that actually landed
/// a `Running` session — spawn the drive loop and the Progress repaint tick
/// (`settings/mail_import.rs::after_snapshot`'s twin).
fn after_snapshot(ctx: &Rc<ExportCtx>, snap: &MailExportSnapshot, should_run: bool) {
    render(ctx, snap);
    if should_run && snap.session_state == Some(ExportSessionState::Running) {
        // Fire-and-forget: `run_export` mutates the machine's own snapshot as it
        // goes (a failure lands on the snapshot's `error` and the session is
        // failed nest-side), and the tick below is what repaints it.
        let machine = Arc::clone(&ctx.machine);
        ctx.rt.spawn(async move {
            if let Err(e) = machine.run_export().await {
                tracing::warn!("[settings/mail_export] run_export: {e:?}");
            }
        });
        start_progress_tick(ctx);
    }
}

/// Repaint the Progress screen from the machine while `run_export` runs — a
/// pure, synchronous snapshot read on the GTK thread, no RPC. Stops as soon as
/// the wizard leaves the Progress step (Done, or a Cancel that unwinds it).
fn start_progress_tick(ctx: &Rc<ExportCtx>) {
    if ctx.ticking.get() {
        return;
    }
    ctx.ticking.set(true);
    let ctx = Rc::clone(ctx);
    glib::timeout_add_local(
        std::time::Duration::from_millis(u64::from(super::mail_import::PROGRESS_TICK_MS)),
        move || {
            let snap = ctx.machine.snapshot();
            let keep_going = snap.step == ExportStep::Progress;
            render(&ctx, &snap);
            if keep_going {
                glib::ControlFlow::Continue
            } else {
                ctx.ticking.set(false);
                glib::ControlFlow::Break
            }
        },
    );
}

/// Render a `MailExportSnapshot` into the page widgets (GTK main thread).
fn render(ctx: &Rc<ExportCtx>, snap: &MailExportSnapshot) {
    let w = &ctx.w;

    super::render_error_label(&w.error_label, snap.error.as_deref());

    // Step-gated group visibility (the Scope step spans two groups).
    w.format_group.set_visible(snap.step == ExportStep::Format);
    w.mailboxes_group
        .set_visible(snap.step == ExportStep::Scope);
    w.scope_group.set_visible(snap.step == ExportStep::Scope);
    w.confirm_group
        .set_visible(snap.step == ExportStep::Confirm);
    w.progress_group
        .set_visible(snap.step == ExportStep::Progress);
    w.done_group.set_visible(snap.step == ExportStep::Done);

    // Format picker (echo-guarded).
    ctx.syncing.set(true);
    if w.format_picker.selected() != index_of(snap.format) {
        w.format_picker.set_selected(index_of(snap.format));
    }
    if w.strip_headers_toggle.is_active() != snap.strip_headers {
        w.strip_headers_toggle.set_active(snap.strip_headers);
    }
    ctx.syncing.set(false);

    // Mailbox multi-select: rebuild the CheckButtons from the snapshot.
    {
        let mut rows = w.mailbox_rows.borrow_mut();
        for cb in rows.drain(..) {
            w.mailboxes_group.remove(&cb);
        }
        for mb in &snap.mailboxes {
            let cb = gtk::CheckButton::with_label(&mb.name);
            cb.set_active(mb.selected);
            // The indexed ui.yaml row id (approved 2026-08-29) plus an EXPLICIT
            // `state` marker: linux's agent falls back to a CheckButton's live
            // checked state and answers `true`/`false`, not the `on`/`off` the
            // cross-app `MailExportActions.mailbox_selected` asserts.
            set_test_id(&cb, ids::MAIL_EXPORT_SCOPE_MAILBOX_ITEM);
            cb.add_css_class(if mb.selected {
                "test-attr-state-on"
            } else {
                "test-attr-state-off"
            });
            {
                let ctx = Rc::clone(ctx);
                let name = mb.name.clone();
                cb.connect_toggled(move |_| {
                    dispatch_action(
                        &ctx,
                        MailExportAction::ToggleMailbox {
                            mailbox: name.clone(),
                        },
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
        fauna_core::format::quota_fraction(snap.exported_count as i64, snap.total_count as i64);
    w.progress_bar.set_fraction(frac);
    w.error_log.set_text(&snap.error_log.join("\n"));
    {
        let mut rows = w.mailbox_progress_rows.borrow_mut();
        for row in rows.drain(..) {
            w.mailbox_progress_group.remove(&row);
        }
        for mp in &snap.mailbox_progress {
            let row = adw::ActionRow::builder().title(&mp.name).build();
            row.add_prefix(&super::marker("mail-export-mailbox-progress-list-item"));
            let name_marker = gtk::Label::new(Some(&mp.name));
            name_marker.set_height_request(1);
            name_marker.set_overflow(gtk::Overflow::Hidden);
            set_test_id(
                &name_marker,
                ids::MAIL_EXPORT_MAILBOX_PROGRESS_LIST_ITEM_NAME,
            );
            row.add_suffix(&name_marker);
            let prog_marker = gtk::Label::new(Some(&format!("{}/{}", mp.exported, mp.total)));
            prog_marker.set_height_request(1);
            prog_marker.set_overflow(gtk::Overflow::Hidden);
            set_test_id(
                &prog_marker,
                ids::MAIL_EXPORT_MAILBOX_PROGRESS_LIST_ITEM_PROGRESS,
            );
            row.add_suffix(&prog_marker);
            w.mailbox_progress_group.add(&row);
            rows.push(row);
        }
    }

    // Done fields.
    w.done_summary.set_text(&done_text(snap));
    w.download_url.set_text(&snap.download_url);
}

fn confirm_text(snap: &MailExportSnapshot) -> String {
    let selected = snap.mailboxes.iter().filter(|m| m.selected).count();
    S::confirm_summary_fmt(&format_label(snap.format), &selected.to_string())
}

fn progress_text(snap: &MailExportSnapshot) -> String {
    S::progress_summary_fmt(
        &snap.exported_count.to_string(),
        &snap.total_count.to_string(),
        &snap.skipped_count.to_string(),
        &snap.errored_count.to_string(),
    )
}

fn done_text(snap: &MailExportSnapshot) -> String {
    match (snap.blob_bytes, snap.saved_archive_path.as_str()) {
        // Once the archive is on disk the summary says WHERE — the visible
        // answer to the Download press (tui's `done_step_elements` shape).
        (Some(b), path) if !path.is_empty() => {
            S::saved_summary_fmt(&format_label(snap.format), &b.to_string(), path)
        }
        (Some(b), _) => S::done_summary_fmt(&format_label(snap.format), &b.to_string()),
        (None, _) => format_label(snap.format),
    }
}

/// Resolve the shared `export_format_label` map through the local i18n runtime.
fn format_label(format: ExportFormat) -> String {
    fauna_client_mail_settings::export_format_label(format).resolve(crate::i18n::strings::lookup)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testid::widget_names;
    use fauna_client_mail_settings::MailboxOption;

    fn a_snapshot() -> MailExportSnapshot {
        MailExportSnapshot {
            step: ExportStep::Confirm,
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
            status: fauna_client_mail_settings::ExportStatus::Idle,
            error: None,
        }
    }

    /// The three wizard summary lines render through the GENERATED i18n
    /// templates, not hard-coded English — linux was the original source of the
    /// duplication found (apple/windows/tui had each copied linux's
    /// inline `format!`s verbatim). Mirrors tui's own pin
    /// (`all_three_summary_lines_render_through_the_i18n_templates`): the
    /// **rendered text** stays the pre-existing byte-identical form (so this
    /// swap is provably zero-behavior-change), and it's compared against
    /// `S::*_summary_fmt` called directly, not re-derived here, so reverting to
    /// a local `format!` goes red.
    #[test]
    fn summary_lines_render_through_the_i18n_templates() {
        for key in [
            "mail_export.confirm_summary_fmt",
            "mail_export.progress_summary_fmt",
            "mail_export.done_summary_fmt",
            "mail_export.saved_summary_fmt",
        ] {
            assert!(
                crate::i18n::strings::lookup(key).is_some(),
                "{key} must resolve — a summary line rendered off a missing key is \
                 hard-coded English wearing an i18n costume"
            );
        }

        let mut snap = a_snapshot();
        snap.mailboxes = vec![MailboxOption {
            name: "INBOX".into(),
            selected: true,
        }];
        assert_eq!(
            confirm_text(&snap),
            "mbox (one file per mailbox — broadest support) · 1 mailbox(es)"
        );
        assert_eq!(
            confirm_text(&snap),
            S::confirm_summary_fmt(&format_label(snap.format), "1")
        );

        let mut snap = a_snapshot();
        snap.exported_count = 3;
        snap.total_count = 10;
        snap.skipped_count = 1;
        snap.errored_count = 2;
        assert_eq!(progress_text(&snap), "3 of 10 · 1 skipped · 2 errored");
        assert_eq!(
            progress_text(&snap),
            S::progress_summary_fmt("3", "10", "1", "2")
        );

        let mut snap = a_snapshot();
        snap.blob_bytes = Some(2048);
        assert_eq!(
            done_text(&snap),
            "mbox (one file per mailbox — broadest support) · 2048 bytes"
        );
        assert_eq!(
            done_text(&snap),
            S::done_summary_fmt(&format_label(snap.format), "2048")
        );

        let mut snap = a_snapshot();
        snap.blob_bytes = None;
        assert_eq!(
            done_text(&snap),
            "mbox (one file per mailbox — broadest support)"
        );

        // After Download the summary names where the archive landed — the
        // visible answer to the press.
        let mut snap = a_snapshot();
        snap.blob_bytes = Some(2048);
        snap.saved_archive_path = "/home/u/Downloads/fauna-export-u-mbox-2026-09-25.zip.zst".into();
        assert_eq!(
            done_text(&snap),
            S::saved_summary_fmt(
                &format_label(snap.format),
                "2048",
                "/home/u/Downloads/fauna-export-u-mbox-2026-09-25.zip.zst"
            )
        );
        assert!(
            done_text(&snap).contains("/home/u/Downloads/"),
            "{}",
            done_text(&snap)
        );
    }

    /// The Export page exposes every static ui.yaml ID for the `mail-export`
    /// wizard. The indexed `mail-export-mailbox-progress-list-item*` rows are
    /// added from the snapshot at render time, so they're not asserted here.
    #[test]
    fn export_page_exposes_static_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let page = build_mail_export_page();
            let names = widget_names(&page);
            for id in [
                "page-heading",
                "error-message",
                "mail-export-format-picker",
                "mail-export-scope-mailboxes",
                "mail-export-scope-date-from",
                "mail-export-scope-date-to",
                "mail-export-scope-strip-headers-toggle",
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
                "wizard-next-button",
                "wizard-back-button",
                "mail-export-mailbox-progress-list",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}",
                );
            }
        });
    }
}
