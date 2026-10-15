//! User-initiated reporting on linux (`moderation.md` § User-initiated
//! reporting → *App surface*, *What the reporter is told*, *Where it lands*):
//! the **report sheet** the three entry verbs open — `feed-post-report-button`
//! (the post ⋯ menu), `dm-message-report-button` (the message ⋯ menu) and
//! `profile-report-button` (an OTHER profile) — and the follow-ups a send runs.
//!
//! Paint only. Every decision is shared Rust: the reason list, the submit gate,
//! whether the include-text checkbox renders, the acknowledgement, the ledger
//! and queue wording are `fauna_client_moderation::report`; the reporter-side
//! hide is `fauna_sync_engine::preference_surfaces::{hide_reported,
//! load_hidden_content}` over the sealed `fauna.state.moderation` record, read
//! by the render verdict through [`crate::content_policy`]. tui
//! (`apps/fauna-tui/src/report.rs`) and web (`ReportHost.svelte`) are the
//! references; this file is the GTK shape of the same sequence.
//!
//! **What a submit does, in order** (`moderation.md` § What the reporter is
//! told, § Corollary):
//! 1. `fauna.moderation.abuse_report.submit` — the report itself. A failure
//!    stops here on the app's `error-message` and the sheet stays open for a
//!    retry.
//! 2. When `report-block-author-checkbox` was ticked, the existing
//!    `fauna.knocks.block` on the author — one gesture, two existing
//!    mechanisms; the nest only records the wish.
//! 3. The reporter-side hide: the subject's id joins the sealed
//!    `hidden_content` list, the stored list the write returns replaces the
//!    render engine's copy, and the open feed / thread repaints so the reported
//!    item reads "You reported this".
//!
//! Steps 2–3 failing does not un-send the report: the acknowledgement
//! (`report-status`) still paints, and the follow-up failure rides
//! `error-message` BESIDE it — never silent.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use fauna_client::NestClient;
use fauna_client_moderation::{ModerationClient, ReportForm, ReportTarget, report};
use fauna_core::localized::LocalizedText;
use fauna_protocol::moderation::{
    AbuseReportMineEntry, AbuseReportQueueEntry, AbuseReportReason, AbuseReportSubmitRequest,
};
use fauna_ui_ids as ids;

use crate::client::FaunaClient;
use crate::i18n::strings::moderation::report as t;

/// The two app-wide banner labels the sheet writes to: the `error-message`
/// every page's failures ride, and the `report-status` acknowledgement beside
/// the page content (outside the sheet, so it outlives the closed dialog).
#[derive(Clone)]
struct Banners {
    error: gtk::Label,
    status: gtk::Label,
}

thread_local! {
    static BANNERS: RefCell<Option<Banners>> = const { RefCell::new(None) };
    /// The wake channels of the surfaces that must repaint when the
    /// reporter-side hide list changes (the open feed, the open thread).
    static REPAINT: RefCell<Vec<async_channel::Sender<()>>> = const { RefCell::new(Vec::new()) };
}

/// Register the app-wide banners (`app.rs`, at window build). Replaces any
/// earlier pair — linux rebuilds the window in process on sign-out and switch.
pub fn register_banners(error: &gtk::Label, status: &gtk::Label) {
    BANNERS.with(|b| {
        *b.borrow_mut() = Some(Banners {
            error: error.clone(),
            status: status.clone(),
        });
    });
}

fn resolve(text: &LocalizedText) -> String {
    text.resolve(crate::i18n::strings::lookup)
}

/// Paint (or clear) the app-wide `error-message` banner.
pub fn show_error(message: Option<&str>) {
    BANNERS.with(|b| {
        if let Some(banners) = b.borrow().as_ref() {
            match message {
                Some(m) => {
                    banners.error.set_text(m);
                    banners.error.set_visible(true);
                }
                None => {
                    banners.error.set_text("");
                    banners.error.set_visible(false);
                }
            }
        }
    });
}

fn show_status(message: Option<&str>) {
    BANNERS.with(|b| {
        if let Some(banners) = b.borrow().as_ref() {
            match message {
                Some(m) => {
                    banners.status.set_text(m);
                    banners.status.set_visible(true);
                }
                None => {
                    banners.status.set_text("");
                    banners.status.set_visible(false);
                }
            }
        }
    });
}

/// Subscribe a surface to hide-list changes: the returned receiver wakes once
/// per change (coalesced), to be consumed with
/// [`crate::async_helper::spawn_wake_loop`] like a manager observer's.
pub fn subscribe_repaint() -> async_channel::Receiver<()> {
    let (tx, rx) = crate::async_helper::snapshot_wake_channel();
    REPAINT.with(|r| r.borrow_mut().push(tx));
    rx
}

/// Close every repaint subscription — the identity-change drop
/// (`crate::actor_scope`). A subscribed view of the outgoing window outlives
/// its `destroy()` (the tree never finalizes), so its loop never sees its root
/// go; closing the channel ends it, where a repaint would otherwise paint the
/// outgoing account's cards under the incoming account's content policy and
/// Guardian Notify counter. The incoming window's views subscribe afresh.
pub fn clear_repaint_subscribers() {
    REPAINT.with(|r| r.borrow_mut().clear());
}

fn request_repaint() {
    REPAINT.with(|r| {
        r.borrow_mut().retain(|tx| {
            let _ = tx.try_send(());
            !tx.is_closed()
        });
    });
}

/// Apply a freshly read or written hide list to the render engine and repaint.
fn apply_hidden(list: Vec<String>) {
    crate::content_policy::set_hidden_content(list);
    request_repaint();
}

/// Read the reporter-side hide list at the post-auth hook, WITHOUT blocking
/// any surface's mount path (web's first run hung the feed behind an awaited
/// load): the list lands on the GTK thread when the account store answers, and
/// the open surfaces repaint then. Best-effort — a hard failure leaves the
/// render without the hide until the next sign-in.
pub fn load_hidden_content(client: &FaunaClient) {
    crate::async_helper::spawn_with_snapshot(
        &client.runtime_handle(),
        move || async move {
            let store = crate::account_runtime::handle_source();
            crate::async_helper::hydrate_with_retry(|| async {
                fauna_sync_engine::preference_surfaces::load_hidden_content(&store)
                    .await
                    .map_err(fauna_sync_engine::preference_surfaces::plane_failure)
            })
            .await
        },
        |read| match read {
            Ok(list) => apply_hidden(list),
            Err(e) => tracing::warn!(
                "hidden_content read failed; the reporter-side hide is inactive: {e}"
            ),
        },
    );
}

/// What the network half of a submit resolved to.
enum Outcome {
    /// The report landed. `hidden` is the stored list after step 3 when it
    /// ran; `followup_error` a step-2/3 failure to surface beside the
    /// acknowledgement.
    Submitted {
        acknowledgement: String,
        blocked: Option<String>,
        hidden: Option<Vec<String>>,
        followup_error: Option<String>,
    },
    /// The submit itself failed.
    Failed(String),
}

async fn run_submit(
    nest: Arc<NestClient>,
    request: AbuseReportSubmitRequest,
    block: Option<String>,
) -> Outcome {
    let hide_id = request.subject.id().to_string();
    let reply = match ModerationClient::new(Arc::clone(&nest))
        .abuse_report_submit(request)
        .await
    {
        Ok(reply) => reply,
        Err(e) => return Outcome::Failed(resolve(&report::report_failed(e.to_string()))),
    };
    let mut followup_error = None;
    let mut blocked = None;
    if let Some(author) = block {
        match fauna_client_contacts::ContactsClient::new(Arc::clone(&nest))
            .knocks_block(author.clone())
            .await
        {
            Ok(_) => blocked = Some(author),
            Err(e) => followup_error = Some(format!("block: {e}")),
        }
    }
    // Never a silent no-op: when the report went and the hide did not, the page
    // says so.
    let store = crate::account_runtime::handle_source();
    let hidden = match fauna_sync_engine::preference_surfaces::hide_reported(&store, &hide_id).await
    {
        Ok(list) => Some(list),
        Err(e) => {
            followup_error.get_or_insert(format!("hide: {e}"));
            None
        }
    };
    Outcome::Submitted {
        acknowledgement: resolve(&report::report_acknowledgement(&reply.routed_to)),
        blocked,
        hidden,
        followup_error,
    }
}

/// The callback a report sheet runs with the author's actor id when the
/// reporter also blocked them.
pub type BlockedHook = Rc<dyn Fn(&str)>;

/// Open the report sheet over `anchor`'s window for `target`.
///
/// `on_blocked` runs on the GTK thread with the author's actor id when the
/// reporter also blocked them, so the page that opened the sheet (a profile)
/// can refresh its own block state.
pub fn open_sheet(
    anchor: &impl IsA<gtk::Widget>,
    target: ReportTarget,
    client: &Rc<FaunaClient>,
    on_blocked: Option<BlockedHook>,
) {
    // A fresh open starts with no stale acknowledgement or error.
    show_status(None);
    show_error(None);

    let target = Rc::new(target);
    let form = Rc::new(RefCell::new(ReportForm::default()));
    let sending = Rc::new(Cell::new(false));
    let view = report::report_sheet_view(&target.subject, target.sealed, &form.borrow());

    let dialog = adw::Dialog::builder()
        .title(resolve(&view.title))
        .content_width(420)
        .build();
    crate::testid::set_test_id(&dialog, ids::REPORT_SHEET);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
    content.set_margin_top(16);
    content.set_margin_bottom(16);
    content.set_margin_start(16);
    content.set_margin_end(16);

    let title = gtk::Label::new(Some(&resolve(&view.title)));
    title.set_halign(gtk::Align::Start);
    title.add_css_class("title-3");
    content.append(&title);

    // The reason picker. Index 0 is the prompt itself — the sheet preselects
    // nothing, so no report is filed under a reason nobody chose.
    let mut labels = vec![resolve(&view.reason_label)];
    labels.extend(view.reasons.iter().map(|o| resolve(&o.label)));
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let reason_select = gtk::DropDown::from_strings(&label_refs);
    crate::testid::set_test_id(&reason_select, ids::REPORT_REASON_SELECT);
    // Each option's reason token (the prompt has none) — the key a test selects
    // by, as on every other app; "Something else" is `other`.
    let mut keys = vec![""];
    keys.extend(view.reasons.iter().map(|o| o.reason.token()));
    crate::testid::set_test_option_keys(&reason_select, &keys);
    content.append(&reason_select);

    let note_label = gtk::Label::new(Some(&resolve(&view.note_label)));
    note_label.set_halign(gtk::Align::Start);
    note_label.add_css_class("dim-label");
    content.append(&note_label);
    let note = gtk::Entry::new();
    crate::testid::set_test_id(&note, ids::REPORT_NOTE_INPUT);
    content.append(&note);

    // Only a sealed subject offers the excerpt — the shared view says so.
    let include_text = view.show_include_text.then(|| {
        let check = gtk::CheckButton::with_label(&resolve(&view.include_text_label));
        crate::testid::set_test_id(&check, ids::REPORT_INCLUDE_TEXT_CHECKBOX);
        content.append(&check);
        check
    });

    let block_author = gtk::CheckButton::with_label(&resolve(&view.block_author_label));
    crate::testid::set_test_id(&block_author, ids::REPORT_BLOCK_AUTHOR_CHECKBOX);
    content.append(&block_author);

    // Why the send is blocked, when it is — form guidance, not a page error.
    let hint = gtk::Label::new(None);
    hint.set_halign(gtk::Align::Start);
    hint.set_wrap(true);
    hint.add_css_class("dim-label");
    content.append(&hint);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label(&resolve(&view.cancel_label));
    crate::testid::set_test_id(&cancel, ids::REPORT_CANCEL_BUTTON);
    let submit = gtk::Button::with_label(&resolve(&view.submit_label));
    submit.add_css_class("suggested-action");
    crate::testid::set_test_id(&submit, ids::REPORT_SUBMIT_BUTTON);
    // Only the submit reaches the nest.
    crate::offline_gate::declare_wire_kind(&submit, "fauna.moderation.abuse_report.submit");
    buttons.append(&cancel);
    buttons.append(&submit);
    content.append(&buttons);
    dialog.set_child(Some(&content));

    // The shared fold is the gate: re-derive the submit control and the hint
    // from the draft after every edit.
    let refresh: Rc<dyn Fn()> = {
        let target = Rc::clone(&target);
        let form = Rc::clone(&form);
        let sending = Rc::clone(&sending);
        let submit = submit.clone();
        let hint = hint.clone();
        Rc::new(move || {
            let view = report::report_sheet_view(&target.subject, target.sealed, &form.borrow());
            submit.set_sensitive(view.can_submit && !sending.get());
            match view.blocked_reason.as_ref() {
                Some(reason) => {
                    hint.set_text(&resolve(reason));
                    hint.set_visible(true);
                }
                None => hint.set_visible(false),
            }
        })
    };
    refresh();

    {
        let form = Rc::clone(&form);
        let refresh = Rc::clone(&refresh);
        let reasons: Vec<AbuseReportReason> = view.reasons.iter().map(|o| o.reason).collect();
        reason_select.connect_selected_notify(move |dd| {
            let picked = (dd.selected() as usize)
                .checked_sub(1)
                .and_then(|i| reasons.get(i).copied());
            form.borrow_mut().reason = picked;
            refresh();
        });
    }
    {
        let form = Rc::clone(&form);
        let refresh = Rc::clone(&refresh);
        note.connect_changed(move |e| {
            form.borrow_mut().note = e.text().to_string();
            refresh();
        });
    }
    if let Some(check) = include_text.as_ref() {
        let form = Rc::clone(&form);
        check.connect_toggled(move |c| form.borrow_mut().include_text = c.is_active());
    }
    {
        let form = Rc::clone(&form);
        block_author.connect_toggled(move |c| form.borrow_mut().block_author = c.is_active());
    }
    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| {
            dialog.close();
        });
    }
    {
        let dialog = dialog.clone();
        let target = Rc::clone(&target);
        let form = Rc::clone(&form);
        let sending = Rc::clone(&sending);
        let refresh = Rc::clone(&refresh);
        let client_for_send = Rc::clone(client);
        submit.connect_clicked(move |_| {
            if sending.get() {
                return;
            }
            // `None` exactly when the view said the send is blocked, so a stale
            // click cannot file a reason-less report.
            let Some(request) = report::report_request(&target, &form.borrow()) else {
                return;
            };
            let block = form
                .borrow()
                .block_author
                .then(|| target.author.clone())
                .flatten();
            sending.set(true);
            refresh();
            let nest = Arc::clone(client_for_send.nest_rpc());
            let dialog = dialog.clone();
            let sending = Rc::clone(&sending);
            let refresh = Rc::clone(&refresh);
            let on_blocked = on_blocked.clone();
            crate::async_helper::spawn_with_snapshot(
                &client_for_send.runtime_handle(),
                move || run_submit(nest, request, block),
                move |outcome| match outcome {
                    Outcome::Failed(message) => {
                        // The sheet stays open for a retry.
                        sending.set(false);
                        refresh();
                        show_error(Some(&message));
                    }
                    Outcome::Submitted {
                        acknowledgement,
                        blocked,
                        hidden,
                        followup_error,
                    } => {
                        dialog.close();
                        show_status(Some(&acknowledgement));
                        if let Some(list) = hidden {
                            apply_hidden(list);
                        }
                        if let (Some(actor), Some(cb)) = (blocked.as_deref(), on_blocked.as_ref()) {
                            cb(actor);
                        }
                        show_error(followup_error.as_deref());
                    }
                },
            );
        });
    }

    // Anchored on the window, not the verb's button: the verb sits in a card
    // that rebuilds on every manager notify, and a dialog must outlive that.
    let window = anchor
        .root()
        .and_then(|root| root.dynamic_cast::<gtk::Widget>().ok());
    dialog.present(window.as_ref().or(Some(anchor.upcast_ref())));
}

/// The report target for a feed post. A gated post was opened through a key,
/// so the nest holds no readable bytes of it — the sealed rule's post arm.
pub fn post_target(post: &fauna_feed::PostSummary) -> ReportTarget {
    ReportTarget::post(
        &post.post_id,
        &post.author,
        &post.body,
        post.gated_tier.is_some() || post.gated_room.is_some(),
    )
}

/// The report target for an account (the OTHER profile).
pub fn actor_target(actor_id: &str) -> ReportTarget {
    ReportTarget::actor(actor_id)
}

/// The report target for a conversation message, off the plane identity the
/// thread already carries. `None` for a message with no plane identity (mail
/// and bridged rails): there is nothing to report it against, so the verb
/// paints nothing.
pub fn message_target(msg: &fauna_conversations::MessageSnapshot) -> Option<ReportTarget> {
    let plane = msg.plane_ref.as_ref()?;
    ReportTarget::message(
        &plane.scope,
        &plane.record_digest,
        msg.sender.person_actor_id().map(|a| a.to_hex()),
        &msg.document.to_plaintext(),
    )
}

// ── The reporter's ledger and the admin queue (paint helpers) ──

/// One reporter-ledger row's text (`moderation-report-item`):
/// `reason · status · short subject id[ · outcome] — destinations`, every word
/// the shared `ledger_row_view` fold.
pub fn ledger_row_text(entry: &AbuseReportMineEntry) -> String {
    let view = report::ledger_row_view(entry);
    let mut text = format!(
        "{} · {} · {}",
        resolve(&view.reason),
        resolve(&view.status),
        fauna_core::format::short_id(entry.subject.id()),
    );
    if let Some(outcome) = view.outcome.as_ref() {
        text.push_str(&format!(" · {}", resolve(outcome)));
    }
    text.push_str(&format!(" — {}", resolve(&view.routed_to)));
    text
}

/// One admin-queue row's text (`admin-nest-report-item`):
/// `reason · <kind> <subject id> · origin · when[ — note][ — “excerpt”]`.
pub fn queue_row_text(entry: &AbuseReportQueueEntry) -> String {
    let view = report::queue_row_view(entry);
    let now_ms = fauna_core::data::Timestamp::now_millis_or_zero() as i64;
    let when = if entry.created_at == 0 {
        String::new()
    } else {
        fauna_core::format::relative_time_text(
            now_ms,
            entry.created_at / 1_000,
            crate::i18n::strings::lookup,
        )
    };
    let mut text = format!(
        "{} · {} {} · {} · {}",
        resolve(&view.reason),
        view.subject_kind,
        view.subject_id,
        resolve(&view.origin),
        when,
    );
    if let Some(note) = view.note.as_deref() {
        text.push_str(&format!(" — {note}"));
    }
    if let Some(excerpt) = view.excerpt.as_deref() {
        text.push_str(&format!(" — “{excerpt}”"));
    }
    text
}

/// The reporter's own ledger, newest first (`fauna.moderation.abuse_report.mine`).
pub async fn read_ledger(nest: Arc<NestClient>) -> Result<Vec<AbuseReportMineEntry>, String> {
    ModerationClient::new(nest)
        .abuse_report_mine()
        .await
        .map(|reply| reply.reports)
        .map_err(|e| format!("load your reports: {e}"))
}

/// Withdraw one open report, then re-read the ledger. The first element is the
/// shared verdict line (`withdraw_verdict`) the page paints beside the ledger.
pub async fn withdraw_and_read(
    nest: Arc<NestClient>,
    report_id: String,
) -> (String, Result<Vec<AbuseReportMineEntry>, String>) {
    let error = ModerationClient::new(Arc::clone(&nest))
        .abuse_report_withdraw(report_id)
        .await
        .err()
        .map(|e| e.to_string());
    let verdict = resolve(&report::withdraw_verdict(error));
    (verdict, read_ledger(nest).await)
}

/// The open reports on this nest (`fauna.moderation.abuse_report.queue`, Admin).
pub async fn read_queue(nest: Arc<NestClient>) -> Result<Vec<AbuseReportQueueEntry>, String> {
    fauna_client_admin::AdminClient::new(nest)
        .abuse_report_queue()
        .await
        .map(|reply| reply.reports)
        .map_err(|e| format!("load reports: {e}"))
}

/// Record one report's outcome, then re-read the queue. The first element is
/// the shared verdict line (`resolve_verdict`).
pub async fn resolve_and_read(
    nest: Arc<NestClient>,
    report_id: String,
    outcome: fauna_protocol::moderation::AbuseReportOutcome,
) -> (String, Result<Vec<AbuseReportQueueEntry>, String>) {
    let error = fauna_client_admin::AdminClient::new(Arc::clone(&nest))
        .abuse_report_resolve(report_id, outcome)
        .await
        .err()
        .map(|e| e.to_string());
    let verdict = resolve(&report::resolve_verdict(outcome, error));
    (verdict, read_queue(nest).await)
}

/// The words the withdraw control paints.
pub fn withdraw_label() -> &'static str {
    t::WITHDRAW
}

/// The ledger's header words.
pub fn ledger_title() -> String {
    resolve(&report::ledger_title())
}

/// The ledger's empty-state words.
pub fn ledger_empty() -> String {
    resolve(&report::ledger_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::moderation::{AbuseReportOutcome, AbuseReportStatus, AbuseReportSubject};

    fn mine(
        status: AbuseReportStatus,
        outcome: Option<AbuseReportOutcome>,
    ) -> AbuseReportMineEntry {
        AbuseReportMineEntry {
            report_id: "r1".into(),
            subject: AbuseReportSubject::Actor {
                actor_id: "ab".repeat(32),
            },
            reason: AbuseReportReason::Impersonation,
            created_at: 0,
            status,
            outcome,
            routed_to: vec!["nest.example".into()],
            extra: Default::default(),
        }
    }

    /// The ledger row the e2e reads: `reason · status · short subject id[ ·
    /// outcome] — destinations`, every word the shared fold's.
    #[test]
    fn a_ledger_row_reads_reason_status_subject_outcome_and_destinations() {
        let short = fauna_core::format::short_id(&"ab".repeat(32));
        let open = ledger_row_text(&mine(AbuseReportStatus::Open, None));
        assert_eq!(
            open,
            format!("Impersonation · Open · {short} — Sent to nest.example")
        );
        let resolved = ledger_row_text(&mine(
            AbuseReportStatus::Resolved,
            Some(AbuseReportOutcome::Acted),
        ));
        assert_eq!(
            resolved,
            format!("Impersonation · Resolved · {short} · Acted on — Sent to nest.example")
        );
    }
}
