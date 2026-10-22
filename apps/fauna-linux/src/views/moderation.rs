//! The standalone **Moderation** page (`moderation-tab` → `moderation` stack
//! child) — the user's view onto why their *own* content was labeled /
//! quarantined / rejected, and their lever to correct it.
//!
//! Presentation over the **union** of two sources (`moderation.md` § Layout &
//! flow): the server `fauna.moderation.actions` obligation rows (`ObligationAction`
//! — the enforcement half: mail-ingest / admin quarantine·reject·label + appeals,
//! plus plaintext-mode social labels) and the client's own post-decrypt **local
//! detections** (the encrypted-mode social-content signal the nest can't produce).
//! The two merge + dedupe through the shared `fauna_client_moderation::merge_queue`
//! into one `moderation-queue`; each [`QueueRow`] carries a `content-label-badge`
//! (the category, via the shared `fauna_core::content_category::content_label_style`
//! map — icon + accent colour + i18n label, drift #157), an *optional* enforcement
//! action (present on server rows, **blank** on local detections), confidence, and
//! a `train-correction-button` (a server row submits `fauna.moderation.train`; a
//! local row removes the client-side false-positive flag).
//!
//! Unified shape: a standalone page on standalone clients, embedded in Settings
//! on web, the **same IDs** either way (`docs/goal/behavior/moderation.md`
//! § Architectural rules 1; mirrors Windows' standalone Moderation page). The
//! spam *preferences* (`spam-moderation-controls`) live on the Settings page,
//! not here — the queue consumes those preferences but does not host them.

use adw::prelude::*;
use fauna_client_moderation::{AppealForm, QueueRow, QueueRowSource};
use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use crate::client::FaunaClient;
use crate::i18n::strings::moderation as mod_strings;
use crate::i18n::strings::settings::moderation_page as mod_page_strings;

/// A `train-correction-button` handler: `(content_id, is_local)`.
type OnCorrect = Rc<dyn Fn(&str, bool)>;

/// An `appeal-button` handler: the row's `content_id`.
type OnAppeal = Rc<dyn Fn(&str)>;

/// An appeal being composed (`moderation.md` § Legal takedown — the
/// transparency triple's second leg). It captures its row's `content_id` at
/// OPEN time: the queue refetches and repaints, so a dispatch keyed on the row
/// index could appeal the wrong decision. The reason lives here, not in the
/// entry widget, so a repaint mid-typing does not lose it.
#[derive(Clone)]
struct AppealDraft {
    content_id: String,
    reason: String,
}

/// The Moderation page's repaintable state: the queue list, the page-level
/// `appeal-status` line, the rows last painted and the open appeal draft.
/// Message-loop handlers repaint through this on
/// `DataMessage::ModerationActionsLoaded` / `AppealSubmitted`.
pub struct ModerationPage {
    list_box: gtk::ListBox,
    /// `appeal-status` — page-level so it survives the form closing on
    /// success, and deliberately NOT the page `error-message`: a refused
    /// appeal is form feedback that must not blank the queue being read.
    status: gtk::Label,
    rows: RefCell<Vec<QueueRow>>,
    draft: RefCell<Option<AppealDraft>>,
    /// The queue row a `dm-message-withheld-badge` tap asked to appeal, by `content_id`:
    /// the next queue read opens its draft ([`queue_appeal`]), so the badge reaches the
    /// one appeal flow.
    pending_appeal: RefCell<Option<String>>,
    client: Rc<FaunaClient>,
}

/// Handles the message loop needs to repaint the queue.
pub struct ModerationViewHandles {
    pub page: Rc<ModerationPage>,
}

/// Build the Moderation page: a header + the scrolled `moderation-queue` list.
/// The queue starts empty; rows are painted by [`update_moderation_queue`] on
/// `fauna.moderation.actions` delivery (so no `FaunaClient` is needed here).
pub fn build_moderation_view(client: &Rc<FaunaClient>) -> (gtk::Box, ModerationViewHandles) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let header = adw::HeaderBar::new();
    let title_label = gtk::Label::new(Some(mod_page_strings::TITLE));
    crate::testid::set_test_id(&title_label, ids::PAGE_HEADING);
    header.set_title_widget(Some(&title_label));
    outer.append(&header);

    // Section caption above the queue (mirrors Windows' "Enforcement Actions").
    let section = gtk::Label::new(Some(mod_strings::ENFORCEMENT_TITLE));
    section.set_halign(gtk::Align::Start);
    section.add_css_class("title-4");
    section.set_margin_top(12);
    section.set_margin_start(12);
    section.set_margin_bottom(6);
    outer.append(&section);

    let queue_list_box = gtk::ListBox::new();
    queue_list_box.set_selection_mode(gtk::SelectionMode::None);
    queue_list_box.add_css_class("boxed-list");
    queue_list_box.set_margin_start(12);
    queue_list_box.set_margin_end(12);
    // The `moderation-queue` component container (scope anchor for e2e).
    crate::testid::set_test_id(&queue_list_box, ids::MODERATION_QUEUE);

    // Empty state: `actions: []` is not an error — render the empty placeholder
    // (moderation.md § Errors & edge cases).
    let placeholder = adw::StatusPage::builder()
        .title(mod_strings::NO_ACTIONS)
        .icon_name("security-high-symbolic")
        .build();
    queue_list_box.set_placeholder(Some(&placeholder));

    // The appeal outcome line, hidden until an attempt (`appeal-status`).
    let appeal_status = gtk::Label::new(None);
    appeal_status.set_halign(gtk::Align::Start);
    appeal_status.set_wrap(true);
    appeal_status.set_margin_start(12);
    appeal_status.set_margin_end(12);
    appeal_status.set_margin_top(6);
    appeal_status.set_visible(false);
    crate::testid::set_test_id(&appeal_status, ids::APPEAL_STATUS);

    // The page scrolls as a whole: the enforcement queue, then the reporter's
    // own ledger beneath it.
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.append(&queue_list_box);
    page.append(&appeal_status);
    page.append(&build_reports_ledger(client));
    let scrolled = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&page)
        .build();
    outer.append(&scrolled);

    let handles = ModerationViewHandles {
        page: Rc::new(ModerationPage {
            list_box: queue_list_box,
            status: appeal_status,
            rows: RefCell::new(Vec::new()),
            draft: RefCell::new(None),
            pending_appeal: RefCell::new(None),
            client: Rc::clone(client),
        }),
    };
    (outer, handles)
}

/// The reporter's own ledger (`moderation-reports-section` — `moderation.md`
/// § User-initiated reporting → *What the reporter is told*): subject, reason,
/// destination(s), status, and a withdraw while the report is open. Every word
/// is the shared `ledger_row_view` fold (`crate::report::ledger_row_text`).
/// Rows paint flat, the queue's own convention, so the e2e addresses them by
/// index; the empty line paints only off a `loaded` bit, so a read still in
/// flight never claims "You have not reported anything".
struct Ledger {
    list: gtk::ListBox,
    empty: gtk::Label,
    status: gtk::Label,
    client: Rc<FaunaClient>,
}

fn build_reports_ledger(client: &Rc<FaunaClient>) -> gtk::Box {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 6);
    section.set_margin_top(18);
    section.set_margin_start(12);
    section.set_margin_end(12);
    section.set_margin_bottom(12);

    let title = gtk::Label::new(Some(&crate::report::ledger_title()));
    title.set_halign(gtk::Align::Start);
    title.add_css_class("title-4");
    crate::testid::set_test_id(&title, ids::MODERATION_REPORTS_SECTION);
    section.append(&title);

    let empty = gtk::Label::new(Some(&crate::report::ledger_empty()));
    empty.set_halign(gtk::Align::Start);
    empty.add_css_class("dim-label");
    empty.set_visible(false);
    section.append(&empty);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    section.append(&list);

    let status = gtk::Label::new(None);
    status.set_halign(gtk::Align::Start);
    status.set_wrap(true);
    status.add_css_class("dim-label");
    status.set_visible(false);
    section.append(&status);

    let ledger = Rc::new(Ledger {
        list,
        empty,
        status,
        client: Rc::clone(client),
    });
    // Read beside the queue on every entry to the page: the page's section
    // maps each time the stack arrives on it.
    {
        let ledger = Rc::clone(&ledger);
        section.connect_map(move |_| reload_ledger(&ledger));
    }
    section
}

fn reload_ledger(ledger: &Rc<Ledger>) {
    let nest = Arc::clone(ledger.client.nest_rpc());
    let ledger = Rc::clone(ledger);
    crate::async_helper::spawn_with_snapshot(
        &ledger.client.runtime_handle(),
        move || crate::report::read_ledger(nest),
        move |read| paint_ledger(&ledger, read, None),
    );
}

fn paint_ledger(
    ledger: &Rc<Ledger>,
    read: Result<Vec<fauna_protocol::moderation::AbuseReportMineEntry>, String>,
    status: Option<String>,
) {
    let entries = match read {
        Ok(entries) => entries,
        Err(e) => {
            // A refused read never paints "You have not reported anything".
            crate::report::show_error(Some(&e));
            return;
        }
    };
    while let Some(child) = ledger.list.first_child() {
        ledger.list.remove(&child);
    }
    ledger.empty.set_visible(entries.is_empty());
    for entry in &entries {
        let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        hbox.set_margin_top(8);
        hbox.set_margin_bottom(8);
        hbox.set_margin_start(12);
        hbox.set_margin_end(12);

        let text = gtk::Label::new(Some(&crate::report::ledger_row_text(entry)));
        text.set_hexpand(true);
        text.set_halign(gtk::Align::Start);
        text.set_xalign(0.0);
        text.set_wrap(true);
        crate::testid::set_test_id(&text, ids::MODERATION_REPORT_ITEM);
        crate::testid::set_test_attr(&text, "status", entry.status.token());
        crate::testid::set_test_attr(&text, "subject", entry.subject.id());
        hbox.append(&text);

        if fauna_client_moderation::report::ledger_row_view(entry).can_withdraw {
            let withdraw = gtk::Button::with_label(crate::report::withdraw_label());
            crate::testid::set_test_id(&withdraw, ids::MODERATION_REPORT_WITHDRAW_BUTTON);
            crate::offline_gate::declare_wire_kind(
                &withdraw,
                "fauna.moderation.abuse_report.withdraw",
            );
            let weak = Rc::downgrade(ledger);
            let report_id = entry.report_id.clone();
            withdraw.connect_clicked(move |b| {
                let Some(ledger) = weak.upgrade() else {
                    return;
                };
                b.set_sensitive(false);
                let nest = Arc::clone(ledger.client.nest_rpc());
                let report_id = report_id.clone();
                crate::async_helper::spawn_with_snapshot(
                    &ledger.client.runtime_handle(),
                    move || crate::report::withdraw_and_read(nest, report_id),
                    move |(verdict, read)| paint_ledger(&ledger, read, Some(verdict)),
                );
            });
            hbox.append(&withdraw);
        }
        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&hbox));
        row.set_activatable(false);
        ledger.list.append(&row);
    }
    match status {
        Some(line) => {
            ledger.status.set_text(&line);
            ledger.status.set_visible(true);
        }
        None => ledger.status.set_visible(false),
    }
}

/// Repaint the queue from a freshly-merged [`QueueRow`] list (the server obligation
/// rows ∪ the client's local detections). Clears the list and rebuilds one row per
/// entry (or shows the empty placeholder).
pub fn update_moderation_queue(page: &Rc<ModerationPage>, rows: &[QueueRow]) {
    *page.rows.borrow_mut() = rows.to_vec();
    open_pending_appeal(page);
    repaint(page);
}

/// Ask for the appeal of the queue row `content_id` names — the
/// `dm-message-withheld-badge` tap (`moderation.md` § Legal takedown → *App render*). The
/// caller then enters the page, whose arrival re-reads the queue;
/// [`update_moderation_queue`] opens the draft on the matching row once it is loaded, so
/// the appeal is the queue's own (one flow, the `appeal-*` ids).
pub fn queue_appeal(page: &Rc<ModerationPage>, content_id: &str) {
    *page.pending_appeal.borrow_mut() = Some(content_id.to_string());
    *page.draft.borrow_mut() = None;
    page.status.set_visible(false);
}

/// Open the draft a badge tap asked for, on the loaded row it names — behind the same
/// `row_is_appealable` gate as the row's own `appeal-button`. A row the queue no longer
/// carries opens nothing.
fn open_pending_appeal(page: &Rc<ModerationPage>) {
    let Some(content_id) = page.pending_appeal.borrow_mut().take() else {
        return;
    };
    let appealable =
        page.rows.borrow().iter().any(|row| {
            row.content_id == content_id && fauna_client_moderation::row_is_appealable(row)
        });
    if appealable {
        *page.draft.borrow_mut() = Some(AppealDraft {
            content_id,
            reason: String::new(),
        });
    }
}

/// Rebuild the queue list from the retained rows, painting the open appeal form
/// (if any) directly under the row it was opened against.
fn repaint(page: &Rc<ModerationPage>) {
    crate::views::layout::clear_list_box_rows(&page.list_box);
    // `train-correction-button` applies a correction — a server row submits a `ham`
    // (not-spam) `fauna.moderation.train`, a local row removes the false-positive
    // flag from the session store (see `FaunaClient::correct_moderation_row`). The
    // closure is shared across rows; each row binds its own `content_id` + whether
    // it is a local detection.
    let on_correct: OnCorrect = {
        let client = Rc::clone(&page.client);
        Rc::new(move |content_id: &str, is_local: bool| {
            client.correct_moderation_row(content_id, is_local)
        })
    };
    // Weak handles: the page owns the list, the list owns the buttons, and the
    // buttons own these closures — a strong `Rc` back would leak the page.
    let on_appeal: OnAppeal = {
        let weak: Weak<ModerationPage> = Rc::downgrade(page);
        Rc::new(move |content_id: &str| {
            if let Some(page) = weak.upgrade() {
                open_appeal(&page, content_id);
            }
        })
    };
    let draft = page.draft.borrow().clone();
    for row in page.rows.borrow().iter() {
        page.list_box.append(&build_queue_row(
            row,
            Rc::clone(&on_correct),
            Rc::clone(&on_appeal),
        ));
        if let Some(draft) = draft.as_ref().filter(|d| d.content_id == row.content_id) {
            page.list_box
                .append(&build_appeal_form(draft, &appeal_callbacks(page)));
        }
    }
}

/// `appeal-button` — open the form on the row, capturing its `content_id`.
fn open_appeal(page: &Rc<ModerationPage>, content_id: &str) {
    *page.draft.borrow_mut() = Some(AppealDraft {
        content_id: content_id.to_string(),
        reason: String::new(),
    });
    page.status.set_visible(false);
    repaint(page);
}

/// `appeal-cancel-button` — drop the draft and the stale status together.
fn cancel_appeal(page: &Rc<ModerationPage>) {
    *page.draft.borrow_mut() = None;
    page.status.set_visible(false);
    repaint(page);
}

/// An appeal dispatch resolved (`DataMessage::AppealSubmitted`): `None` =
/// recorded for review, `Some(e)` = the error the shared `appeal_verdict`
/// words. Success CLOSES the form (the decision is with an admin now; the queue
/// row stays — appeals are additive history); a failure KEEPS it open, reason
/// intact, so it can be fixed and resent.
pub fn finish_appeal(page: &Rc<ModerationPage>, error: Option<String>) {
    let recorded = error.is_none();
    page.status.set_text(
        &fauna_client_moderation::appeal_verdict(error).resolve(crate::i18n::strings::lookup),
    );
    page.status.set_visible(true);
    if recorded {
        *page.draft.borrow_mut() = None;
    }
    repaint(page);
}

/// What the appeal form's three controls call back into.
struct AppealCallbacks {
    on_reason: Rc<dyn Fn(&str)>,
    on_submit: Rc<dyn Fn()>,
    on_cancel: Rc<dyn Fn()>,
}

fn appeal_callbacks(page: &Rc<ModerationPage>) -> AppealCallbacks {
    let weak = Rc::downgrade(page);
    let on_reason: Rc<dyn Fn(&str)> = {
        let weak = weak.clone();
        Rc::new(move |text: &str| {
            if let Some(page) = weak.upgrade()
                && let Some(draft) = page.draft.borrow_mut().as_mut()
            {
                draft.reason = text.to_string();
            }
        })
    };
    let on_submit: Rc<dyn Fn()> = {
        let weak = weak.clone();
        Rc::new(move || {
            let Some(page) = weak.upgrade() else {
                return;
            };
            let Some(draft) = page.draft.borrow().clone() else {
                return;
            };
            // The shared fold gates the dispatch, so a reasonless or over-long
            // appeal never reaches the wire.
            if !draft_view(&draft).can_submit {
                return;
            }
            page.client.submit_appeal(draft.content_id, draft.reason);
        })
    };
    let on_cancel: Rc<dyn Fn()> = Rc::new(move || {
        if let Some(page) = weak.upgrade() {
            cancel_appeal(&page);
        }
    });
    AppealCallbacks {
        on_reason,
        on_submit,
        on_cancel,
    }
}

/// The shared `appeal_form_view` fold over a draft.
fn draft_view(draft: &AppealDraft) -> fauna_client_moderation::AppealFormView {
    fauna_client_moderation::appeal_form_view(&AppealForm {
        content_id: draft.content_id.clone(),
        reason: draft.reason.clone(),
    })
}

/// The open appeal form (`moderation.md` § Legal takedown — the transparency
/// triple's second leg), as one non-activatable list row under its decision.
/// Every gate and sentence is the shared `appeal_form_view` fold — the submit
/// control is insensitive, with the fold's stated reason beside it, until a
/// reason is typed — so no app grows its own idea of a valid appeal.
fn build_appeal_form(draft: &AppealDraft, cb: &AppealCallbacks) -> gtk::ListBoxRow {
    let view = draft_view(draft);
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 6);
    vbox.set_margin_top(8);
    vbox.set_margin_bottom(8);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    let summary = gtk::Label::new(Some(&view.summary.resolve(crate::i18n::strings::lookup)));
    summary.set_halign(gtk::Align::Start);
    summary.set_wrap(true);
    summary.add_css_class("caption");
    summary.add_css_class("dim-label");
    vbox.append(&summary);

    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some(mod_strings::APPEAL_REASON_LABEL));
    entry.set_text(&draft.reason);
    crate::testid::set_test_id(&entry, ids::APPEAL_REASON_INPUT);
    vbox.append(&entry);

    // The fold's stated reason, painted as chrome beside the dead control.
    let blocked = gtk::Label::new(None);
    blocked.set_halign(gtk::Align::Start);
    blocked.set_wrap(true);
    blocked.add_css_class("caption");
    blocked.add_css_class("dim-label");
    if let Some(reason) = &view.blocked_reason {
        blocked.set_text(&reason.resolve(crate::i18n::strings::lookup));
    }
    blocked.set_visible(view.blocked_reason.is_some());
    vbox.append(&blocked);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let cancel = gtk::Button::with_label(mod_strings::APPEAL_CANCEL);
    cancel.add_css_class("flat");
    crate::testid::set_test_id(&cancel, ids::APPEAL_CANCEL_BUTTON);
    {
        let on_cancel = Rc::clone(&cb.on_cancel);
        cancel.connect_clicked(move |_| on_cancel());
    }
    buttons.append(&cancel);

    let submit = gtk::Button::with_label(&view.submit_label.resolve(crate::i18n::strings::lookup));
    submit.set_sensitive(view.can_submit);
    crate::testid::set_test_id(&submit, ids::APPEAL_SUBMIT_BUTTON);
    {
        let on_submit = Rc::clone(&cb.on_submit);
        submit.connect_clicked(move |b| {
            // One dispatch per click; the result repaints a fresh form.
            b.set_sensitive(false);
            on_submit();
        });
    }
    buttons.append(&submit);
    vbox.append(&buttons);

    // Typing re-folds the draft in place (no repaint — that would drop focus).
    {
        let on_reason = Rc::clone(&cb.on_reason);
        let content_id = draft.content_id.clone();
        let submit = submit.clone();
        let blocked = blocked.clone();
        entry.connect_changed(move |e| {
            let reason = e.text().to_string();
            on_reason(&reason);
            let view = draft_view(&AppealDraft {
                content_id: content_id.clone(),
                reason,
            });
            submit.set_sensitive(view.can_submit);
            match &view.blocked_reason {
                Some(r) => blocked.set_text(&r.resolve(crate::i18n::strings::lookup)),
                None => blocked.set_text(""),
            }
            blocked.set_visible(view.blocked_reason.is_some());
        });
    }

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&vbox));
    row.set_activatable(false);
    row.set_selectable(false);
    row
}

/// Build the shared `content-label-badge` widget for `category`: icon + accent
/// colour + i18n label, entirely through `fauna_core::content_category::content_label_style`
/// so no client hard-codes the category→style map (moderation.md § Where logic
/// lives, drift #157). The **one** place linux paints this badge — the
/// moderation queue, feed post-cards, and DM bubbles all call it, so none of
/// them can drift from each other. The category text carries no per-page ID
/// in the spec (moderation.md § Element IDs), but the testid rides on the
/// text label so e2e reads the clean category.
pub(crate) fn build_content_label_badge(category: &str) -> gtk::Box {
    content_label_badge_parts(category).0
}

/// [`build_content_label_badge`] plus the id-carrying label inside it — the
/// widget a queue row hangs its `confidence` / `action` attributes on.
fn content_label_badge_parts(category: &str) -> (gtk::Box, gtk::Label) {
    let style = fauna_core::content_category::content_label_style(category);
    let badge_box = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    badge_box.add_css_class(&format!("content-label-{category}"));

    let icon = gtk::Label::new(Some(&style.icon));
    icon.add_css_class("caption");
    badge_box.append(&icon);

    let badge_text = style.label.resolve(crate::i18n::strings::lookup);
    let badge = gtk::Label::new(None);
    // Pango markup paints the shared accent colour inline (no per-widget
    // CssProvider, no hard-coded linux colour) while `Label::text()` still
    // returns the bare label for e2e / the widget test.
    badge.set_markup(&format!(
        "<span foreground=\"{}\">{}</span>",
        style.accent,
        gtk::glib::markup_escape_text(&badge_text)
    ));
    badge.add_css_class("caption-heading");
    crate::testid::set_test_id(&badge, ids::CONTENT_LABEL_BADGE);
    badge_box.append(&badge);
    (badge_box, badge)
}

/// One `moderation-queue` row: category badge, an *optional* action, confidence,
/// and a truncated content ref, with a `train-correction-button`. `on_correct` is
/// invoked with the row's `content_id` + whether it is a local detection when the
/// button is clicked — kept as a callback (not the `FaunaClient`) so the renderer is
/// unit-testable without a live client (the `render_mode_fields` idiom). A server
/// row renders its enforcement action; a **local** detection's action column is
/// blank (`moderation.md` § Layout & flow — "a local-only row shows a blank action
/// column"; never fabricate an action, § Don't do these).
fn build_queue_row(row: &QueueRow, on_correct: OnCorrect, on_appeal: OnAppeal) -> gtk::ListBoxRow {
    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    hbox.set_margin_top(8);
    hbox.set_margin_bottom(8);
    hbox.set_margin_start(12);
    hbox.set_margin_end(12);

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 4);
    vbox.set_hexpand(true);

    // Top line: the content-label badge + the action taken.
    let top_line = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    // Confidence as a whole-percent via the shared half-up rounding contract
    // (`fauna_core::format::confidence_percent`); the wire forbids floats, so
    // `confidence_per_mille` is 0..=1000.
    let pct = fauna_core::format::confidence_percent(row.confidence_per_mille);
    // The enforcement action taken, via the shared discriminant→label map
    // (co-located with the enum in `fauna_core::obligation`). Server rows only —
    // a local detection carries no `action`, so its action column stays blank.
    let action_text = row.action.map(|action| {
        fauna_core::obligation::obligation_action_label(action)
            .resolve(crate::i18n::strings::lookup)
    });

    // `content-label-badge` — see `build_content_label_badge`'s doc comment. It
    // also publishes the confidence and action this row paints as `confidence`
    // / `action` attributes (`moderation.md` § Element IDs → *Queue row facts*):
    // the painted lines have no id of their own.
    let (badge_box, badge) = content_label_badge_parts(&row.category);
    crate::testid::set_test_attr_text(&badge, "confidence", &format!("{pct}%"));
    if let Some(text) = &action_text {
        crate::testid::set_test_attr_text(&badge, "action", text);
    }
    // A row another nest delivered names where the decision was made — the
    // shared `issued_by` suffix on the action line, and the `issuer` fact on
    // the badge (`moderation.md` § Legal takedown → *The author homed on
    // another nest*).
    if let Some(nest) = &row.issuer_nest {
        crate::testid::set_test_attr_text(&badge, "issuer", nest);
    }
    top_line.append(&badge_box);

    if let Some(action_text) = action_text {
        let action_text = match &row.issued_by {
            Some(issued_by) => format!(
                "{action_text} · {}",
                issued_by.resolve(crate::i18n::strings::lookup)
            ),
            None => action_text,
        };
        let action_label = gtk::Label::new(Some(&action_text));
        action_label.add_css_class("caption");
        action_label.add_css_class("dim-label");
        top_line.append(&action_label);
    }

    vbox.append(&top_line);

    // Content ref (type + truncated id) — an opaque handle, shown so the user
    // can correlate the row with their content.
    let id_short = fauna_core::format::short_id(&row.content_id);
    let ref_label = gtk::Label::new(Some(&format!("{} · {}", row.content_type, id_short)));
    ref_label.set_halign(gtk::Align::Start);
    ref_label.add_css_class("caption");
    ref_label.add_css_class("dim-label");
    ref_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    vbox.append(&ref_label);

    let conf_label = gtk::Label::new(Some(&format!("{pct}% {}", mod_strings::CONFIDENCE)));
    conf_label.set_halign(gtk::Align::Start);
    conf_label.add_css_class("caption");
    conf_label.add_css_class("dim-label");
    vbox.append(&conf_label);

    hbox.append(&vbox);

    // `train-correction-button` — a single correction: the queue lists items the
    // classifier *flagged*, so the correction is "this was a false positive →
    // not spam" (`verdict: "ham"`), training the caller's Bayesian model
    // (moderation.md § User actions). Mirrors Windows' single "Correct" button.
    let correct_btn = gtk::Button::with_label(mod_strings::CORRECT);
    correct_btn.add_css_class("flat");
    correct_btn.set_valign(gtk::Align::Center);
    crate::testid::set_test_id(&correct_btn, ids::TRAIN_CORRECTION_BUTTON);
    {
        let content_id = row.content_id.clone();
        let is_local = matches!(row.source, QueueRowSource::Local);
        correct_btn.connect_clicked(move |_| on_correct(&content_id, is_local));
    }
    hbox.append(&correct_btn);

    // `appeal-button` — the transparency triple's second leg. Offered on
    // nest-issued (server) rows only, by the shared `row_is_appealable` rule: a
    // local detection records nobody's decision, so its lever is the correction
    // button above, and it paints no dead appeal control.
    if fauna_client_moderation::row_is_appealable(row) {
        let appeal_btn = gtk::Button::with_label(mod_strings::APPEAL);
        appeal_btn.add_css_class("flat");
        appeal_btn.set_valign(gtk::Align::Center);
        crate::testid::set_test_id(&appeal_btn, ids::APPEAL_BUTTON);
        let content_id = row.content_id.clone();
        appeal_btn.connect_clicked(move |_| on_appeal(&content_id));
        hbox.append(&appeal_btn);
    }

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&hbox));
    row.set_selectable(false);
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sample [`QueueRow`]. `action = Some(_)` is a server obligation row;
    /// `action = None` is a client-side local detection (blank action column).
    fn sample(category: &str, action: Option<u8>, conf: u16) -> QueueRow {
        QueueRow {
            content_id: "ab".repeat(32),
            content_type: "post".into(),
            category: category.into(),
            confidence_per_mille: conf,
            action,
            timestamp: 1_700_000_000_000_000,
            source: if action.is_some() {
                QueueRowSource::Server
            } else {
                QueueRowSource::Local
            },
            issuer_nest: None,
            issued_by: None,
        }
    }

    #[test]
    fn sample_has_canonical_category() {
        // Guards the test helper against an off-list category slipping in.
        let a = sample("spam", Some(2), 880);
        assert_eq!(
            fauna_core::content_category::content_label_style(&a.category)
                .label
                .key,
            "moderation.category.spam"
        );
    }

    /// Walk a widget subtree collecting `(test_id, label_text)` for every
    /// `gtk::Label`/`gtk::Button` carrying a widget name.
    fn collect_ids(widget: &gtk::Widget, out: &mut Vec<(String, String)>) {
        let name = widget.widget_name().to_string();
        if !name.is_empty() {
            let text = widget
                .downcast_ref::<gtk::Label>()
                .map(|l| l.text().to_string())
                .or_else(|| {
                    widget
                        .downcast_ref::<gtk::Button>()
                        .map(|b| b.label().map(|s| s.to_string()).unwrap_or_default())
                })
                .unwrap_or_default();
            out.push((name, text));
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            collect_ids(&c, out);
            child = c.next_sibling();
        }
    }

    #[test]
    fn row_renders_badge_and_train_button() {
        crate::testid::run_on_gtk_thread(|| {
            let row = build_queue_row(
                &sample("phishing", Some(1), 925),
                Rc::new(|_, _| {}),
                Rc::new(|_| {}),
            );
            let mut ids = Vec::new();
            collect_ids(row.upcast_ref::<gtk::Widget>(), &mut ids);

            // The badge resolves the canonical category label (not the raw `category`
            // wire value) through the shared map + i18n.
            let badge = ids
                .iter()
                .find(|(id, _)| id == "content-label-badge")
                .expect("content-label-badge present");
            assert_eq!(badge.1, "Phishing");

            // The single correction button carries the canonical id + the shared label.
            let btn = ids
                .iter()
                .find(|(id, _)| id == "train-correction-button")
                .expect("train-correction-button present");
            assert_eq!(btn.1, "Correct");
        });
    }

    /// Walk a widget subtree collecting the text of **every** `gtk::Label`
    /// (regardless of whether it carries a test id) — the action label is not
    /// test-id'd, so [`collect_ids`] can't see it.
    fn collect_all_label_texts(widget: &gtk::Widget, out: &mut Vec<String>) {
        if let Some(l) = widget.downcast_ref::<gtk::Label>() {
            out.push(l.text().to_string());
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            collect_all_label_texts(&c, out);
            child = c.next_sibling();
        }
    }

    /// F3 (legal-takedown badge verify; tracked internally § F3). A server obligation row for a
    /// **taken-down** post carries `category="illegal"` (the enforcement
    /// descriptor the nest writes at `moderation_handlers.rs` `post_legal_\
    /// takedown_txn`), deliberately OUTSIDE the canonical-5 classifier categories
    /// — the real signal is the `TakenDown` action label. The queue row must
    /// render **gracefully**: the off-list category degrades to the shared
    /// `content_label_style` `Other` fallback (grey badge, capitalized "Illegal"),
    /// never a panic and never a dropped row, and the `TakenDown` enforcement
    /// label "Removed under legal obligation" still renders alongside it.
    #[test]
    fn legal_takedown_row_renders_off_list_category_and_action_label() {
        crate::testid::run_on_gtk_thread(|| {
            // A server TakenDown obligation (action = 7 = ObligationAction::TakenDown)
            // carrying the off-list `illegal` enforcement descriptor at max confidence.
            let taken_down_action = fauna_core::obligation::ObligationAction::TakenDown as u8;
            let row = build_queue_row(
                &sample("illegal", Some(taken_down_action), 1000),
                Rc::new(|_, _| {}),
                Rc::new(|_| {}),
            );
            let mut ids = Vec::new();
            collect_ids(row.upcast_ref::<gtk::Widget>(), &mut ids);

            // The row is NOT dropped: its `content-label-badge` (and correction
            // button) rendered, and the badge degrades to the shared `Other` fallback
            // — the capitalized raw string, not a canonical-5 label and not a panic.
            let badge = ids
                .iter()
                .find(|(id, _)| id == "content-label-badge")
                .expect("content-label-badge present for an off-list category");
            assert_eq!(badge.1, "Illegal");
            assert!(ids.iter().any(|(id, _)| id == "train-correction-button"));

            // The `TakenDown` enforcement label renders alongside the badge (server
            // row → non-blank action column via the shared discriminant→label map).
            let taken_down_label =
                fauna_core::obligation::obligation_action_label(taken_down_action)
                    .resolve(crate::i18n::strings::lookup);
            assert_eq!(taken_down_label, "Removed under legal obligation");
            let mut texts = Vec::new();
            collect_all_label_texts(row.upcast_ref::<gtk::Widget>(), &mut texts);
            assert!(
                texts.iter().any(|t| t == &taken_down_label),
                "TakenDown action label {taken_down_label:?} must render in the row; got {texts:?}"
            );
        });
    }

    /// The badge publishes the confidence and action the row paints as
    /// `confidence` / `action` attributes (`moderation.md` § Element IDs →
    /// *Queue row facts*); a local detection publishes no `action`.
    #[test]
    fn the_badge_carries_the_rows_confidence_and_action() {
        fn badge_of(widget: &gtk::Widget) -> Option<gtk::Widget> {
            if widget.widget_name() == "content-label-badge" {
                return Some(widget.clone());
            }
            let mut child = widget.first_child();
            while let Some(c) = child {
                if let Some(found) = badge_of(&c) {
                    return Some(found);
                }
                child = c.next_sibling();
            }
            None
        }
        crate::testid::run_on_gtk_thread(|| {
            let taken_down = fauna_core::obligation::ObligationAction::TakenDown as u8;
            let row = build_queue_row(
                &sample("illegal", Some(taken_down), 880),
                Rc::new(|_, _| {}),
                Rc::new(|_| {}),
            );
            let badge = badge_of(row.upcast_ref()).expect("content-label-badge present");
            assert_eq!(
                crate::testid::test_attr_text(&badge, "confidence").as_deref(),
                Some("88%")
            );
            assert_eq!(
                crate::testid::test_attr_text(&badge, "action").as_deref(),
                Some("Removed under legal obligation")
            );

            let local = build_queue_row(
                &sample("spam", None, 720),
                Rc::new(|_, _| {}),
                Rc::new(|_| {}),
            );
            let badge = badge_of(local.upcast_ref()).expect("content-label-badge present");
            assert_eq!(
                crate::testid::test_attr_text(&badge, "confidence").as_deref(),
                Some("72%")
            );
            assert_eq!(crate::testid::test_attr_text(&badge, "action"), None);
        });
    }

    /// A row another nest delivered names its issuer (`moderation.md` § Legal
    /// takedown → *The author homed on another nest*): the shared `issued_by`
    /// suffix rides on the action line (no id of its own), and the badge
    /// publishes the issuer's domain as the `issuer` fact; this nest's own
    /// decision names none.
    #[test]
    fn a_delivered_row_names_its_issuer() {
        fn badge_of(w: &gtk::Widget) -> Option<gtk::Widget> {
            if w.widget_name() == "content-label-badge" {
                return Some(w.clone());
            }
            let mut child = w.first_child();
            while let Some(c) = child {
                if let Some(b) = badge_of(&c) {
                    return Some(b);
                }
                child = c.next_sibling();
            }
            None
        }
        crate::testid::run_on_gtk_thread(|| {
            let taken_down = fauna_core::obligation::ObligationAction::TakenDown as u8;
            let label = fauna_core::obligation::obligation_action_label(taken_down)
                .resolve(crate::i18n::strings::lookup);
            let mut delivered = sample("illegal", Some(taken_down), 880);
            delivered.issuer_nest = Some("issuer.example".into());
            delivered.issued_by = Some(fauna_core::localized::LocalizedText::key_arg(
                fauna_client_moderation::detections::ISSUED_BY_KEY,
                "nest",
                "issuer.example",
            ));
            let row = build_queue_row(&delivered, Rc::new(|_, _| {}), Rc::new(|_| {}));
            let suffix = delivered
                .issued_by
                .as_ref()
                .unwrap()
                .resolve(crate::i18n::strings::lookup);
            assert!(suffix.contains("issuer.example"), "{suffix:?}");
            let mut texts = Vec::new();
            collect_all_label_texts(row.upcast_ref::<gtk::Widget>(), &mut texts);
            let line = format!("{label} · {suffix}");
            assert!(texts.contains(&line), "{line:?} not in {texts:?}");
            let badge = badge_of(row.upcast_ref()).expect("content-label-badge present");
            assert_eq!(
                crate::testid::test_attr_text(&badge, "issuer").as_deref(),
                Some("issuer.example")
            );

            let own = build_queue_row(
                &sample("illegal", Some(taken_down), 880),
                Rc::new(|_, _| {}),
                Rc::new(|_| {}),
            );
            let badge = badge_of(own.upcast_ref()).expect("content-label-badge present");
            assert_eq!(crate::testid::test_attr_text(&badge, "issuer"), None);
            let mut texts = Vec::new();
            collect_all_label_texts(own.upcast_ref::<gtk::Widget>(), &mut texts);
            assert!(texts.contains(&label), "{label:?} not in {texts:?}");
        });
    }

    #[test]
    fn local_detection_row_has_blank_action_and_train_button() {
        crate::testid::run_on_gtk_thread(|| {
            // A local detection (action = None) still renders the badge + confidence +
            // train button, but its action column is blank — never a fabricated action
            // (`moderation.md` § Layout & flow / § Don't do these).
            let obligation_labels: Vec<String> = (0u8..=4)
                .map(|a| {
                    fauna_core::obligation::obligation_action_label(a)
                        .resolve(crate::i18n::strings::lookup)
                })
                .collect();
            let row = build_queue_row(
                &sample("spam", None, 720),
                Rc::new(|_, _| {}),
                Rc::new(|_| {}),
            );
            let mut ids = Vec::new();
            collect_ids(row.upcast_ref::<gtk::Widget>(), &mut ids);

            // Badge + train button present, exactly as a server row.
            assert!(ids.iter().any(|(id, _)| id == "content-label-badge"));
            assert!(ids.iter().any(|(id, _)| id == "train-correction-button"));
            // No obligation-action label was rendered anywhere in the row (blank column).
            for (_, text) in &ids {
                assert!(
                    !obligation_labels.iter().any(|l| l == text),
                    "local row must not render an enforcement action; found {text:?}"
                );
            }
        });
    }

    /// First widget in the subtree carrying `name` (its test id).
    fn find_named(widget: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
        if widget.widget_name() == name {
            return Some(widget.clone());
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            if let Some(found) = find_named(&c, name) {
                return Some(found);
            }
            child = c.next_sibling();
        }
        None
    }

    fn noop_callbacks() -> AppealCallbacks {
        AppealCallbacks {
            on_reason: Rc::new(|_| {}),
            on_submit: Rc::new(|| {}),
            on_cancel: Rc::new(|| {}),
        }
    }

    /// The appeal affordance is offered per ROW KIND, not per page: a nest-issued
    /// row paints `appeal-button`, a local detection (no nest-side decision behind
    /// it) paints none — the shared `row_is_appealable` rule, rendered.
    #[test]
    fn only_a_server_row_paints_the_appeal_button() {
        crate::testid::run_on_gtk_thread(|| {
            let server = build_queue_row(
                &sample("illegal", Some(7), 1000),
                Rc::new(|_, _| {}),
                Rc::new(|_| {}),
            );
            assert!(find_named(server.upcast_ref(), "appeal-button").is_some());
            let local = build_queue_row(
                &sample("spam", None, 720),
                Rc::new(|_, _| {}),
                Rc::new(|_| {}),
            );
            assert!(find_named(local.upcast_ref(), "appeal-button").is_none());
        });
    }

    /// Clicking the handle hands back its OWN row's `content_id` — the id the
    /// draft then captures at open time.
    #[test]
    fn the_appeal_button_hands_back_its_rows_content_id() {
        crate::testid::run_on_gtk_thread(|| {
            let opened = Rc::new(RefCell::new(None::<String>));
            let sink = Rc::clone(&opened);
            let row = build_queue_row(
                &sample("illegal", Some(7), 1000),
                Rc::new(|_, _| {}),
                Rc::new(move |id| *sink.borrow_mut() = Some(id.to_string())),
            );
            let button = find_named(row.upcast_ref(), "appeal-button")
                .and_downcast::<gtk::Button>()
                .expect("appeal-button is a button");
            button.emit_clicked();
            assert_eq!(opened.borrow().as_deref(), Some("ab".repeat(32).as_str()));
        });
    }

    /// The open form's submit control follows the shared fold: dead (with the
    /// fold's stated reason painted) until a reason is typed, live after, dead
    /// again once the reason is cleared — a reasonless appeal never reaches the
    /// wire.
    #[test]
    fn the_appeal_form_blocks_until_a_reason_is_typed() {
        crate::testid::run_on_gtk_thread(|| {
            let form = build_appeal_form(
                &AppealDraft {
                    content_id: "ab".repeat(32),
                    reason: String::new(),
                },
                &noop_callbacks(),
            );
            let submit =
                find_named(form.upcast_ref(), "appeal-submit-button").expect("submit present");
            let entry = find_named(form.upcast_ref(), "appeal-reason-input")
                .and_downcast::<gtk::Entry>()
                .expect("reason input is an entry");
            assert!(find_named(form.upcast_ref(), "appeal-cancel-button").is_some());
            assert!(
                !submit.is_sensitive(),
                "a reasonless appeal is never submittable"
            );

            entry.set_text("I hold the licence for this recording.");
            assert!(submit.is_sensitive(), "a reasoned appeal submits");

            entry.set_text("   ");
            assert!(!submit.is_sensitive(), "a blank reason blocks again");
        });
    }

    /// The reason typed is reported back so the page can keep it across a
    /// repaint, and an open form is seeded from the draft it was built from.
    #[test]
    fn the_appeal_form_reports_and_restores_the_reason() {
        crate::testid::run_on_gtk_thread(|| {
            let typed = Rc::new(RefCell::new(String::new()));
            let sink = Rc::clone(&typed);
            let cb = AppealCallbacks {
                on_reason: Rc::new(move |t| *sink.borrow_mut() = t.to_string()),
                on_submit: Rc::new(|| {}),
                on_cancel: Rc::new(|| {}),
            };
            let form = build_appeal_form(
                &AppealDraft {
                    content_id: "ab".repeat(32),
                    reason: "seeded".into(),
                },
                &cb,
            );
            let entry = find_named(form.upcast_ref(), "appeal-reason-input")
                .and_downcast::<gtk::Entry>()
                .expect("entry");
            assert_eq!(entry.text().as_str(), "seeded");
            entry.set_text("edited");
            assert_eq!(typed.borrow().as_str(), "edited");
        });
    }

    /// A withheld badge's tap opens the appeal on the queue row it names once the
    /// queue is loaded — under the matched row, once — and a row the queue no longer
    /// carries (or a local detection) opens nothing.
    #[test]
    fn a_withheld_badge_tap_opens_the_appeal_on_its_queue_row_once_loaded() {
        crate::testid::run_on_gtk_thread(|| {
            let (tx, _rx) = crate::client::ui_channel();
            let machine = fauna_launch_machine::LaunchMachine::new(
                std::sync::Arc::new(fauna_launch_machine::NullObserver),
                std::sync::Arc::new(fauna_launch_machine::InMemoryPersistence::new()),
            );
            let client = Rc::new(FaunaClient::new(
                "http://127.0.0.1:1".to_string(),
                "11".repeat(32),
                tx,
                machine,
            ));
            let (_view, handles) = build_moderation_view(&client);
            let page = handles.page;
            let mut other = sample("spam", Some(1), 900);
            other.content_id = "cd".repeat(32);
            let withheld = sample("illegal", Some(7), 1000);

            queue_appeal(&page, &withheld.content_id);
            assert!(
                page.draft.borrow().is_none(),
                "nothing opens before the read"
            );
            update_moderation_queue(&page, &[other.clone(), withheld.clone()]);
            let draft = page.draft.borrow().clone().expect("the appeal opens");
            assert_eq!(draft.content_id, withheld.content_id);
            assert!(page.pending_appeal.borrow().is_none(), "asked once");

            // A row the queue no longer carries opens nothing.
            *page.draft.borrow_mut() = None;
            queue_appeal(&page, &"ef".repeat(32));
            update_moderation_queue(&page, &[withheld]);
            assert!(page.draft.borrow().is_none());
        });
    }
}
