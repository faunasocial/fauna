//! The standalone **Moderation** page (`moderation-tab` → the `moderation`
//! view) — the user's window onto why their *own* content was labeled /
//! quarantined / rejected, and their lever to correct it.
//!
//! Presentation over the **union** of two sources (`moderation.md` § Layout &
//! flow): the server `fauna.moderation.actions` obligation rows (the enforcement
//! half — mail-ingest / admin quarantine·reject·label + appeals, plus the
//! legal-takedown rows) and the client's own post-decrypt **local detections**
//! (the encrypted-mode social-content signal the nest cannot produce, since it
//! never classifies social content — `content-scoring.md` § Architectural
//! rules). The two merge + dedupe through the shared
//! [`fauna_client_moderation::merge_queue`] — the one place that union lives, so
//! no client can drift from another.
//!
//! Each [`QueueRow`] paints a `content-label-badge` (the category, via the
//! shared `fauna_core::content_category::content_label_style` map — no
//! hard-coded category strings or styling here, `moderation.md` § Where logic
//! lives), an *optional* enforcement action (present on server rows, **blank**
//! on local detections — never fabricated, § Don't do these), the content ref,
//! the confidence as a whole percent through the shared half-up
//! [`fauna_core::format::confidence_percent`] contract, and a
//! `train-correction-button`.
//!
//! Unified shape: a standalone page on the standalone apps, embedded in Settings
//! on web, the **same IDs** either way (`moderation.md` § Architectural rules 1).
//! The spam *preferences* (`spam-moderation-controls`) live on Settings →
//! Privacy — the queue consumes them but does not host them.
//!
//! Row ids are painted **FLAT** (no `.within(…)` scope), because
//! `tests/e2e-unified/actions/moderation.py` addresses them flat —
//! `count("train-correction-button")` / `click("train-correction-button",
//! index=i)`. That question has no house default; read the shared action file
//! every time (the `labeler_catalog` vs `mail_lists` split).

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_mail_settings::MailSettingsMachine;
use fauna_client_moderation::{
    AppealForm, ModerationClient, QueueRow, QueueRowSource, merge_queue,
};
use fauna_conversations::ConversationsSession;
use fauna_i18n::strings::moderation as t;
use fauna_i18n::strings::settings::moderation_page as page_t;
use fauna_protocol::moderation::AbuseReportMineEntry;

use crate::app::App;
use crate::element::{Element, Field, Gesture};
use crate::pages::Page;
use crate::wizard::localized;

/// Page state: the merged queue. The two *sources* are not owned here — the
/// server half is fetched per visit, and the local half is read from the live
/// [`ConversationsSession`] (which owns the shared `LocalDetectionStore`), so
/// this page holds only the merge result.
#[derive(Default)]
pub struct ModerationState {
    /// The live client, `None` pre-login (the page then paints its empty shell).
    nest: Option<Arc<NestClient>>,
    /// The merged, newest-first queue — server rows ∪ local detections.
    pub rows: Vec<QueueRow>,
    /// The open appeal draft, if the user has opened one — at most one at a
    /// time, so the form's ids are unindexed (unlike `appeal-button`, which is
    /// per-row).
    pub appeal: Option<AppealDraft>,
    /// The last appeal's outcome line (`appeal-status`), page-level so it
    /// survives the form closing on success.
    pub appeal_status: Option<String>,
    /// The reporter's own ledger (`moderation-reports-section` —
    /// `moderation.md` § What the reporter is told): the reports this user
    /// made, newest first, as `fauna.moderation.abuse_report.mine` returns
    /// them.
    pub reports: Vec<AbuseReportMineEntry>,
    /// Whether the ledger read has resolved — the empty state paints only off
    /// this bit, never off an empty list (`ui/README.md` § *List pages:
    /// loading is not empty*).
    pub reports_loaded: bool,
    /// The last withdraw's outcome line, worded by the shared
    /// `withdraw_verdict`.
    pub withdraw_status: Option<String>,
}

/// An appeal the user is composing against one queue row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppealDraft {
    /// The row's index in [`ModerationState::rows`] — what the form paints
    /// under, so an appeal reads as belonging to the decision above it.
    pub index: usize,
    /// Captured at open time: the row's `content_id` is what the dispatch
    /// carries, so a refetch that reorders the queue cannot redirect an appeal
    /// the user already started at a different row.
    pub content_id: String,
    /// Why the decision should be reviewed.
    pub reason: String,
}

/// This page's editable fields (`crate::element::Field::Moderation`).
/// `Hash` because `Field` is a focus-map key (the sibling field enums' derive
/// set, `tui.md` § The page-module contract).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModerationField {
    /// `appeal-reason-input`.
    AppealReason,
}

/// Read an editable field.
pub fn field(state: &ModerationState, f: &ModerationField) -> String {
    match f {
        ModerationField::AppealReason => state
            .appeal
            .as_ref()
            .map(|d| d.reason.clone())
            .unwrap_or_default(),
    }
}

/// Write an editable field. A keystroke with no open draft is dropped rather
/// than resurrecting one — the input only exists while the form is open.
pub fn set_field(state: &mut ModerationState, f: ModerationField, value: String) {
    match f {
        ModerationField::AppealReason => {
            if let Some(draft) = state.appeal.as_mut() {
                draft.reason = value;
            }
        }
    }
}

/// Build the page state at the post-auth hook. Unlike notifications there is no
/// login-time prefetch: the queue is re-read on every entry (below), and a
/// client that has just authenticated has no local detections yet either.
pub fn init(nest: Arc<NestClient>) -> ModerationState {
    ModerationState {
        nest: Some(nest),
        // Struct-update: this state grows, and two branches independently
        // adding a field then merge cleanly.
        ..Default::default()
    }
}

/// The nav-edge refresh. Takes the whole [`App`] rather than just this page's
/// state because the queue is a union across two owners: the server rows come
/// from this page's `nest`, and the local detections live on the conversations
/// page's live [`ConversationsSession`]. Returns the op; the caller runs it (the
/// await-vs-spawn duality every other page's `nav_enter_op` keeps).
///
/// Re-read on **every** entry, never cached: server obligation rows can land
/// while no client is looking (an admin action, a legal takedown), and the local
/// half grows with every inbound message the conversations receive loop
/// decrypts.
pub fn nav_enter_op(app: &App) -> Option<Op> {
    Some(Op::Refresh {
        nest: app.moderation.nest.clone()?,
        session: app.conversations.real_session.clone(),
    })
}

/// The page's one gesture. Both row kinds share the button — what it *means*
/// differs by source, which is why the row's own source rides in the gesture
/// rather than being re-derived at apply time (`moderation.md` § User actions).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// `train-correction-button` on the row at this queue index, carrying
    /// that row's own [`QueueRowSource`] — [`wire_kind`](Action::wire_kind)
    /// needs it and only [`apply_local`] has it, which is why it rides here
    /// rather than being re-derived at apply time.
    Correct {
        index: usize,
        source: QueueRowSource,
    },
    /// `appeal-button` on the row at this queue index — opens the appeal form
    /// under it. Offered only where the shared
    /// [`fauna_client_moderation::row_is_appealable`] says so.
    OpenAppeal { index: usize },
    /// `appeal-cancel-button` — drop the draft without dispatching.
    CancelAppeal,
    /// `appeal-submit-button` — dispatch the open draft.
    SubmitAppeal,
    /// `moderation-report-withdraw-button` — withdraw one of the user's OPEN
    /// reports, by id (the ledger row's own, so a refetch that reorders the
    /// list cannot redirect it).
    WithdrawReport { report_id: String },
}

impl Action {
    /// The wire kind this gesture issues — the offline gate's input
    /// (`crate::element::Gesture::wire_kind`). Answers per the carried row
    /// source: a **server** row submits `fauna.moderation.train`
    /// (`OnlineOnly`), so it greys with no nest; a **local** row trains the
    /// sealed tier-1 model on this device and never touches the nest, so it
    /// stays live offline (the same discriminant apple's `correctButton`
    /// gates on: `row.source == .local` skips the server-only gate).
    pub fn wire_kind(&self) -> Option<&'static str> {
        match self {
            Action::Correct {
                source: QueueRowSource::Server,
                ..
            } => Some("fauna.moderation.train"),
            Action::Correct {
                source: QueueRowSource::Local,
                ..
            } => None,
            // Opening and cancelling the form are local UI state; only the
            // dispatch touches the nest, so only it greys offline.
            Action::OpenAppeal { .. } | Action::CancelAppeal => None,
            Action::SubmitAppeal => Some("fauna.moderation.appeal"),
            Action::WithdrawReport { .. } => Some("fauna.moderation.abuse_report.withdraw"),
        }
    }
}

/// Local half of the gesture → its network half.
///
/// ⚠ The index addresses **`state.rows`**, the same list [`elements`] paints —
/// they cannot diverge here because this page applies no filter. A page that
/// paints a *filtered* view of an index-addressed list must carry the snapshot
/// index in the action instead, or it silently mutates the wrong row (the
/// cross-app trap the `labeler-catalog` slice recorded).
pub fn apply_local(app: &mut App, action: Action) -> Option<Op> {
    match action {
        Action::Correct { index, source } => {
            let row = app.moderation.rows.get(index)?;
            Some(Op::Correct {
                nest: app.moderation.nest.clone()?,
                session: app.conversations.real_session.clone(),
                machine: app.settings.mail_machine(),
                content_id: row.content_id.clone(),
                is_local: matches!(source, QueueRowSource::Local),
            })
        }
        Action::OpenAppeal { index } => {
            let row = app.moderation.rows.get(index)?;
            // Refuse to open on a row the shared rule says carries no decision
            // — belt to the paint-time braces, so a stale gesture (a refetch
            // that turned row N local between paint and actuation) cannot
            // start an appeal the nest will only refuse.
            if !fauna_client_moderation::row_is_appealable(row) {
                return None;
            }
            app.moderation.appeal = Some(AppealDraft {
                index,
                content_id: row.content_id.clone(),
                reason: String::new(),
            });
            app.moderation.appeal_status = None;
            None
        }
        Action::CancelAppeal => {
            app.moderation.appeal = None;
            app.moderation.appeal_status = None;
            None
        }
        Action::SubmitAppeal => {
            let draft = app.moderation.appeal.as_ref()?;
            // The shared fold is the gate: the button is painted live only when
            // it says so, and this re-asks rather than trusting the paint.
            let view = fauna_client_moderation::appeal_form_view(&AppealForm {
                content_id: draft.content_id.clone(),
                reason: draft.reason.clone(),
            });
            if !view.can_submit {
                return None;
            }
            Some(Op::Appeal {
                nest: app.moderation.nest.clone()?,
                content_id: draft.content_id.clone(),
                reason: draft.reason.trim().to_string(),
            })
        }
        Action::WithdrawReport { report_id } => {
            app.moderation.withdraw_status = None;
            Some(Op::WithdrawReport {
                nest: app.moderation.nest.clone()?,
                report_id,
            })
        }
    }
}

/// The network half.
pub enum Op {
    Refresh {
        nest: Arc<NestClient>,
        session: Option<Arc<ConversationsSession>>,
    },
    /// A correction on one row. A **server** row runs the shared
    /// `MailSettingsMachine::train_moderation_correction` with a `ham` verdict
    /// (the queue lists what the classifier *flagged*, so the correction is
    /// "this was a false positive"): the sealed model train when possible, then
    /// always `fauna.moderation.train`. A **local** row has no
    /// server row to train against — the nest cannot read client-only encrypted
    /// content — so it trains the sealed tier-1 model over the retained
    /// plaintext and drops the client-side flag, exactly as linux does.
    Correct {
        nest: Arc<NestClient>,
        session: Option<Arc<ConversationsSession>>,
        machine: Option<Arc<MailSettingsMachine>>,
        content_id: String,
        is_local: bool,
    },
    /// Appeal one enforcement decision (`moderation.md` § Legal takedown — the
    /// transparency triple's second leg).
    ///
    /// Deliberately carries **no** session, unlike [`Op::Correct`]: an appeal
    /// changes nothing the queue renders — the row stays, appeals being
    /// additive history (`moderation.md` § Persistence) — so there is no
    /// refetch-after-mutate to feed, and the observable effect is
    /// `appeal-status`.
    Appeal {
        nest: Arc<NestClient>,
        content_id: String,
        reason: String,
    },
    /// Withdraw one open report, then re-read the ledger — the row's status
    /// flip is the observable effect.
    WithdrawReport {
        nest: Arc<NestClient>,
        report_id: String,
    },
}

/// What an [`Op`] resolved to; folded back by [`apply_outcome`].
pub enum Outcome {
    /// The union queue and the reporter's ledger, read together on every
    /// entry. The ledger half fails on its own: a nest that refuses it still
    /// shows the enforcement queue.
    Loaded(Vec<QueueRow>, Result<Vec<AbuseReportMineEntry>, String>),
    Failed(String),
    /// A withdraw resolved (`None` = done), with the re-read ledger.
    Withdrawn(Option<String>, Result<Vec<AbuseReportMineEntry>, String>),
    /// An appeal dispatch resolved — `None` = recorded, `Some(e)` = the error
    /// to word. Deliberately NOT `Failed`: a refused appeal is form feedback on
    /// `appeal-status`, not a page-level `error-message` that would blank the
    /// queue the user is reading.
    Appealed(Option<String>),
}

impl std::fmt::Debug for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Outcome::Loaded(rows, reports) => f
                .debug_tuple("Loaded")
                .field(&rows.len())
                .field(&reports.as_ref().map(Vec::len))
                .finish(),
            Outcome::Failed(e) => f.debug_tuple("Failed").field(e).finish(),
            Outcome::Appealed(e) => f.debug_tuple("Appealed").field(e).finish(),
            Outcome::Withdrawn(e, _) => f.debug_tuple("Withdrawn").field(e).finish(),
        }
    }
}

impl Op {
    pub async fn run(self) -> Outcome {
        match self {
            Op::Refresh { nest, session } => fetch(nest, session).await,
            Op::Correct {
                nest,
                session,
                machine,
                content_id,
                is_local,
            } => {
                if is_local {
                    if let Some(session) = &session {
                        // Read the retained plaintext BEFORE dropping the flag —
                        // the detection row and the message store are independent,
                        // but the read-then-clear order keeps that non-obvious.
                        // `None` once the message aged out of the bounded store;
                        // the correction is then flag-removal only.
                        let body = session.moderation_message_body(content_id.clone());
                        session.moderation_remove_local_detection(content_id.clone());
                        if let (Some(text), Some(machine)) = (body, machine)
                            && !text.trim().is_empty()
                        {
                            // Fire-and-forget + silent, the mark-as-spam
                            // convention: a degraded outcome (mail not enabled /
                            // a nest without sealed-at-rest) is a no-op with no
                            // server fallback, and must never blank the queue.
                            let _ = machine.train_spam_model_client(text, false).await;
                        }
                    }
                } else {
                    // The shared two-half flow: the sealed model train when
                    // possible, then ALWAYS `fauna.moderation.train` (the read
                    // gate + report capture), whose error is the flow's. With no
                    // machine (an undecodable secret) there is no model half to
                    // run — the nest half still reports the correction.
                    let result = match &machine {
                        Some(machine) => machine
                            .train_moderation_correction(content_id, false)
                            .await
                            .map(|_model_half| ())
                            .map_err(|e| e.to_string()),
                        None => ModerationClient::new(Arc::clone(&nest))
                            .train(content_id, "ham")
                            .await
                            .map(|_reply| ())
                            .map_err(|e| e.to_string()),
                    };
                    if let Err(e) = result {
                        return Outcome::Failed(format!("train correction: {e}"));
                    }
                }
                // The refetch IS the observable effect either way (the reply is a
                // bare ack) — the contacts refetch-after-mutate convention.
                fetch(nest, session).await
            }
            Op::Appeal {
                nest,
                content_id,
                reason,
            } => {
                let client = ModerationClient::new(nest);
                match client.appeal(content_id, reason).await {
                    Ok(_) => Outcome::Appealed(None),
                    Err(e) => Outcome::Appealed(Some(e.to_string())),
                }
            }
            Op::WithdrawReport { nest, report_id } => {
                let client = ModerationClient::new(nest);
                let error = client
                    .abuse_report_withdraw(report_id)
                    .await
                    .err()
                    .map(|e| e.to_string());
                Outcome::Withdrawn(error, fetch_reports(&client).await)
            }
        }
    }
}

/// The reporter's own ledger read.
async fn fetch_reports(
    client: &ModerationClient<Arc<NestClient>>,
) -> Result<Vec<AbuseReportMineEntry>, String> {
    client
        .abuse_report_mine()
        .await
        .map(|reply| reply.reports)
        .map_err(|e| format!("load your reports: {e}"))
}

/// The union read: the server obligation queue ∪ the session's local detections,
/// merged + deduped by the shared [`merge_queue`].
///
/// A missing session is **not** an error — it is the honest pre-MLS-session
/// state (no conversations engine yet ⇒ no local detections). It is also the one
/// way this page can silently under-report, so it degrades to the server half
/// rather than to an error the user cannot act on.
async fn fetch(nest: Arc<NestClient>, session: Option<Arc<ConversationsSession>>) -> Outcome {
    let client = ModerationClient::new(nest);
    let reply = match client.actions().await {
        Ok(r) => r,
        Err(e) => return Outcome::Failed(format!("load moderation queue: {e}")),
    };
    let local = session
        .map(|s| s.moderation_local_detections())
        .unwrap_or_default();
    let reports = fetch_reports(&client).await;
    Outcome::Loaded(merge_queue(&reply.actions, &local), reports)
}

/// Fold an [`Outcome`] back into the page. A failure lands on the page's
/// canonical `error-message`; a load clears it (the page shows fresh truth).
pub fn apply_outcome(app: &mut App, outcome: Outcome) {
    match outcome {
        Outcome::Loaded(rows, reports) => {
            app.moderation.rows = rows;
            app.errors.remove(&Page::Moderation);
            apply_reports(app, reports);
        }
        Outcome::Withdrawn(error, reports) => {
            app.moderation.withdraw_status = Some(localized(
                &fauna_client_moderation::report::withdraw_verdict(error),
            ));
            apply_reports(app, reports);
        }
        Outcome::Failed(message) => {
            app.errors.insert(Page::Moderation, message);
        }
        Outcome::Appealed(error) => {
            // Success closes the form (the decision is now with an admin);
            // a failure keeps it open so the reason can be fixed and resent.
            if error.is_none() {
                app.moderation.appeal = None;
            }
            app.moderation.appeal_status =
                Some(localized(&fauna_client_moderation::appeal_verdict(error)));
        }
    }
}

/// Fold a ledger read. A failure keeps whatever the ledger last showed and
/// lands on `error-message`; it never flips `reports_loaded`, so a refused
/// read cannot paint "You have not reported anything".
fn apply_reports(app: &mut App, reports: Result<Vec<AbuseReportMineEntry>, String>) {
    match reports {
        Ok(reports) => {
            app.moderation.reports = reports;
            app.moderation.reports_loaded = true;
        }
        Err(e) => {
            app.errors.insert(Page::Moderation, e);
        }
    }
}

/// The whole page: the enforcement queue, then the reporter's own ledger.
pub fn elements(app: &App) -> Vec<Element> {
    let mut out = queue_elements(app);
    out.extend(ledger_elements(&app.moderation));
    out
}

/// The reporter's ledger (`moderation-reports-section` — `moderation.md`
/// § What the reporter is told): subject, reason, destination(s), status, and
/// withdraw while open. Every word is the shared `ledger_row_view` fold.
/// Rows paint FLAT, the queue's own convention, so `count` /
/// `click(…, index=i)` address them.
fn ledger_elements(st: &ModerationState) -> Vec<Element> {
    use fauna_client_moderation::report;
    let mut out = vec![Element::label(
        ids::MODERATION_REPORTS_SECTION,
        localized(&report::ledger_title()),
    )];
    if st.reports_loaded && st.reports.is_empty() {
        out.push(Element::chrome(localized(&report::ledger_empty())));
    }
    for entry in &st.reports {
        let view = report::ledger_row_view(entry);
        let mut text = format!(
            "{} · {} · {}",
            localized(&view.reason),
            localized(&view.status),
            fauna_core::format::short_id(entry.subject.id()),
        );
        if let Some(outcome) = view.outcome.as_ref() {
            text.push_str(&format!(" · {}", localized(outcome)));
        }
        text.push_str(&format!(" — {}", localized(&view.routed_to)));
        out.push(
            Element::label(ids::MODERATION_REPORT_ITEM, text)
                .attr("status", entry.status.token())
                .attr("subject", entry.subject.id()),
        );
        if view.can_withdraw {
            out.push(Element::gesture_button(
                ids::MODERATION_REPORT_WITHDRAW_BUTTON,
                t::report::WITHDRAW,
                true,
                Gesture::Moderation(Action::WithdrawReport {
                    report_id: entry.report_id.clone(),
                }),
            ));
        }
    }
    if let Some(status) = st.withdraw_status.as_ref() {
        out.push(Element::chrome(status.clone()));
    }
    out
}

/// The ordered ui.yaml element list: the page heading, the `moderation-queue`
/// container (a real element, not decoration — the driver asserts it is visible,
/// and it paints even when the queue is empty so an empty queue is the empty
/// state rather than a missing page), then one row block per [`QueueRow`].
///
/// An empty `actions: []` is **not** an error (`moderation.md` § Errors & edge
/// cases) — it paints the placeholder line.
fn queue_elements(app: &App) -> Vec<Element> {
    let st = &app.moderation;
    let mut out = vec![
        Element::label(ids::PAGE_HEADING, page_t::TITLE),
        // The component container + its section caption in one element (the
        // `report-share-published-list` shape): the scope anchor e2e reads.
        Element::label(ids::MODERATION_QUEUE, t::ENFORCEMENT_TITLE),
    ];
    if st.rows.is_empty() {
        out.push(Element::chrome(t::NO_ACTIONS));
    }
    for (index, row) in st.rows.iter().enumerate() {
        out.extend(row_elements(index, row));
        // The open appeal form paints directly under the row it is against, so
        // an appeal always reads as belonging to the decision above it.
        if st.appeal.as_ref().is_some_and(|d| d.index == index) {
            out.extend(appeal_elements(st));
        }
    }
    if let Some(status) = st.appeal_status.as_ref() {
        out.push(Element::label(ids::APPEAL_STATUS, status.clone()));
    }
    out
}

/// The open appeal form (`moderation.md` § Legal takedown — the transparency
/// triple's second leg). Paint only: whether the submit control is live, and
/// why not when it is not, is the shared
/// [`fauna_client_moderation::appeal_form_view`] fold, so no app grows its own
/// idea of what a valid appeal is.
fn appeal_elements(st: &ModerationState) -> Vec<Element> {
    let Some(draft) = st.appeal.as_ref() else {
        return Vec::new();
    };
    let view = fauna_client_moderation::appeal_form_view(&AppealForm {
        content_id: draft.content_id.clone(),
        reason: draft.reason.clone(),
    });
    let mut els = vec![
        Element::chrome(localized(&view.summary)),
        Element::input(
            ids::APPEAL_REASON_INPUT,
            draft.reason.clone(),
            Field::Moderation(ModerationField::AppealReason),
        )
        .labelled(t::APPEAL_REASON_LABEL),
        Element::gesture_button(
            ids::APPEAL_SUBMIT_BUTTON,
            localized(&view.submit_label),
            view.can_submit,
            Gesture::Moderation(Action::SubmitAppeal),
        ),
        Element::gesture_button(
            ids::APPEAL_CANCEL_BUTTON,
            t::APPEAL_CANCEL,
            true,
            Gesture::Moderation(Action::CancelAppeal),
        ),
    ];
    if let Some(reason) = view.blocked_reason.as_ref() {
        // The disabled submit control owes its reason, as chrome — form
        // guidance, not a page error.
        els.push(Element::chrome(localized(reason)));
    }
    els
}

/// Build the `content-label-badge` element for a wire `category` — icon + label
/// entirely through the shared `fauna_core::content_category::content_label_style`
/// map, so tui hard-codes neither the canonical 5 nor their styling. An off-list
/// category (a legal-takedown row's `illegal`) degrades to the capitalized raw
/// string rather than panicking or dropping the row.
///
/// The **one** place tui paints this badge — the moderation queue, the feed
/// post-card and the DM bubble all call it, so none of them can drift from each
/// other (linux extracted `build_content_label_badge` for exactly this reason).
/// Callers holding a `labels` LIST go through [`content_label_badge`] instead.
pub fn content_label_badge_for_category(category: &str) -> Element {
    let style = fauna_core::content_category::content_label_style(category);
    Element::label(
        ids::CONTENT_LABEL_BADGE,
        format!(
            "{} {}",
            style.icon,
            style.label.resolve(fauna_i18n::strings::lookup)
        ),
    )
}

/// The badge for a `labels` list — the highest-confidence verdict via the shared
/// `primary_content_label` pick, or `None` when nothing was classified. The
/// entry point for the feed post-card (`PostSummary.labels`) and the DM bubble
/// (`MessageSnapshot.labels`), so both agree on which of several labels wins.
pub fn content_label_badge(
    labels: &[fauna_core::content_category::ContentLabelEntry],
) -> Option<Element> {
    fauna_core::content_category::primary_content_label(labels)
        .map(|entry| content_label_badge_for_category(&entry.category))
}

/// One queue row. `content-label-badge` + `train-correction-button` are the only
/// spec'd ids (`moderation.md` § Element IDs — the action/ref/confidence text
/// carries none), so the rest paints as chrome.
fn row_elements(index: usize, row: &QueueRow) -> Vec<Element> {
    // The queue row addresses the badge by its own single `category` (a merged
    // row carries exactly one), so it calls the category helper directly rather
    // than the list one.
    let mut els = vec![content_label_badge_for_category(&row.category)];

    // Server rows only — a local detection carries no `action`, so its action
    // column stays blank. Never fabricate one.
    if let Some(action) = row.action {
        els.push(Element::chrome(
            fauna_core::obligation::obligation_action_label(action)
                .resolve(fauna_i18n::strings::lookup),
        ));
    }

    let pct = fauna_core::format::confidence_percent(row.confidence_per_mille);
    els.push(Element::chrome(format!(
        "{} · {} — {pct}% {}",
        row.content_type,
        fauna_core::format::short_id(&row.content_id),
        t::CONFIDENCE,
    )));

    els.push(Element::gesture_button(
        ids::TRAIN_CORRECTION_BUTTON,
        t::CORRECT,
        true,
        Gesture::Moderation(Action::Correct {
            index,
            source: row.source,
        }),
    ));

    // The appeal affordance — server rows only, per the shared rule. A local
    // detection has no nest-side decision behind it to appeal (its lever is the
    // correction button above), so it paints no button at all rather than a
    // dead one: an appeal is not an affordance this row withholds, it is one
    // that does not apply to it.
    if fauna_client_moderation::row_is_appealable(row) {
        els.push(Element::gesture_button(
            ids::APPEAL_BUTTON,
            t::APPEAL,
            true,
            Gesture::Moderation(Action::OpenAppeal { index }),
        ));
    }
    els
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `action = Some(_)` is a server obligation row; `action = None` is a
    /// client-side local detection (blank action column).
    fn row(category: &str, action: Option<u8>, conf: u16) -> QueueRow {
        QueueRow {
            content_id: "ab".repeat(32),
            content_type: "post".into(),
            category: category.into(),
            confidence_per_mille: conf,
            action,
            timestamp: 0,
            source: if action.is_some() {
                QueueRowSource::Server
            } else {
                QueueRowSource::Local
            },
        }
    }

    /// The empty queue still paints its container — that is what makes an empty
    /// queue read as the empty state rather than a page that failed to load
    /// (`moderation.md` § Errors & edge cases; the e2e asserts visibility).
    #[test]
    fn the_empty_page_paints_the_queue_container_and_a_placeholder() {
        let app = crate::app::tests::test_app();
        let els = queue_elements(&app);
        let ids: Vec<&str> = els.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["page-heading", "moderation-queue", ""]);
        assert_eq!(els[2].text, t::NO_ACTIONS);
    }

    /// A server row paints badge + action + ref/confidence + the correct button.
    #[test]
    fn a_server_row_paints_its_enforcement_action() {
        let mut app = crate::app::tests::test_app();
        // action 7 = TakenDown (the one obligation action production mints).
        app.moderation.rows = vec![row("spam", Some(7), 875)];
        let els = queue_elements(&app);
        let ids: Vec<&str> = els.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "page-heading",
                "moderation-queue",
                "content-label-badge",
                "",
                "",
                "train-correction-button",
                // A nest-issued decision carries the appeal handle too.
                "appeal-button",
            ],
        );
        assert_eq!(
            els[3].text,
            fauna_core::obligation::obligation_action_label(7).resolve(fauna_i18n::strings::lookup),
            "the action column carries the shared discriminant→label map",
        );
        assert!(
            els[4].text.contains("88%"),
            "875 per-mille rounds half-up to 88%, the shared contract: {:?}",
            els[4].text,
        );
    }

    /// A local detection's action column is **blank** — never fabricated
    /// (`moderation.md` § Don't do these). The row is otherwise identical, so the
    /// renderer branches only on `action` being Some/None.
    #[test]
    fn a_local_detection_paints_no_action_column() {
        let mut app = crate::app::tests::test_app();
        app.moderation.rows = vec![row("spam", None, 500)];
        let els = queue_elements(&app);
        let ids: Vec<&str> = els.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "page-heading",
                "moderation-queue",
                "content-label-badge",
                "",
                "train-correction-button",
            ],
            "one fewer chrome line than a server row (the blank action column) \
             and no appeal-button: there is no nest-side decision to appeal",
        );
    }

    /// An off-list category degrades to the capitalized raw string and the row
    /// still renders in full — never a panic, never a dropped row. This is the
    /// legal-takedown shape (`category="illegal"`, whose real signal is the
    /// `TakenDown` action label).
    #[test]
    fn an_off_list_category_still_paints_a_full_row() {
        let mut app = crate::app::tests::test_app();
        app.moderation.rows = vec![row("illegal", Some(7), 1000)];
        let els = queue_elements(&app);
        let badge = els
            .iter()
            .find(|e| e.id == "content-label-badge")
            .expect("an off-list category still paints its badge");
        assert!(
            badge.text.contains("Illegal"),
            "capitalized raw string, not a canonical-5 label: {:?}",
            badge.text,
        );
        assert!(els.iter().any(|e| e.id == "train-correction-button"));
    }

    /// The correction gesture addresses the row by its index in the SAME list
    /// `elements` paints — the wrong-row trap this page avoids by not filtering.
    #[test]
    fn each_row_correction_carries_its_own_index() {
        let mut app = crate::app::tests::test_app();
        app.moderation.rows = vec![row("spam", Some(7), 100), row("nsfw", None, 200)];
        let els = queue_elements(&app);
        let actions: Vec<Action> = els
            .iter()
            .filter(|e| e.id == "train-correction-button")
            .filter_map(|e| match &e.role {
                crate::element::Role::Button(Gesture::Moderation(a)) => Some(a.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            actions,
            vec![
                Action::Correct {
                    index: 0,
                    source: QueueRowSource::Server,
                },
                Action::Correct {
                    index: 1,
                    source: QueueRowSource::Local,
                },
            ],
            "each row's button carries its OWN index AND source — the wrong-row trap, \
             and wire_kind's only input",
        );
    }

    /// The offline gate's whole point for this gesture: a server row's
    /// correction submits `fauna.moderation.train` (`OnlineOnly`) and greys
    /// with no nest, but a local row's never touches the nest and stays live
    /// — the two directions apple's `correctButton` already discriminates on
    /// the same `row.source`.
    #[test]
    fn a_server_rows_correction_greys_offline_a_local_rows_does_not() {
        let mut app = crate::app::tests::authed_app();
        app.page = Page::Moderation;
        app.connection = fauna_ws_substrate::supervisor::ConnectionState::Disconnected;
        app.moderation.rows = vec![row("spam", Some(7), 900), row("nsfw", None, 500)];

        let buttons: Vec<_> = app
            .page_elements()
            .into_iter()
            .filter(|e| e.id == "train-correction-button")
            .collect();
        assert_eq!(buttons.len(), 2);
        assert!(
            !buttons[0].enabled,
            "the server row's correction is OnlineOnly and must grey with no nest"
        );
        assert!(
            buttons[1].enabled,
            "the local row's correction never touches the nest, so it must stay live"
        );
    }

    /// The shared badge builder is the ONE place tui paints this element, so the
    /// queue, the feed post-card and the DM bubble cannot drift: a labels list
    /// resolves to the highest-confidence entry, and an empty list to no badge
    /// at all (the caller then paints nothing).
    #[test]
    fn the_shared_badge_picks_the_highest_confidence_label() {
        use fauna_core::content_category::ContentLabelEntry;
        let entry = |category: &str, conf: u16| ContentLabelEntry {
            category: category.to_string(),
            confidence_per_mille: conf,
        };

        assert!(
            content_label_badge(&[]).is_none(),
            "nothing classified ⇒ no badge element at all"
        );

        let badge = content_label_badge(&[entry("nsfw", 200), entry("phishing", 900)])
            .expect("a classified message carries a badge");
        assert_eq!(badge.id, "content-label-badge");
        assert!(
            badge.text.contains(
                &fauna_core::content_category::content_label_style("phishing")
                    .label
                    .resolve(fauna_i18n::strings::lookup)
            ),
            "the 900 entry wins over the 200 one: {:?}",
            badge.text,
        );
    }

    /// The union is the shared `merge_queue`, not a tui-local re-implementation:
    /// a server row and a local detection on the SAME `content_id` dedupe to one
    /// row, and the server row wins (it carries the action).
    #[test]
    fn the_queue_is_the_shared_deduplicated_union() {
        use fauna_client_moderation::LocalDetection;
        let server = vec![fauna_protocol::moderation::ObligationAction {
            id: 1,
            content_type: "post".into(),
            content_id: "cd".repeat(32),
            category: "spam".into(),
            confidence_per_mille: 900,
            action: 7,
            timestamp: 10,
            extra: Default::default(),
        }];
        let local = vec![LocalDetection {
            content_id: "cd".repeat(32),
            content_type: "message".into(),
            category: "spam".into(),
            confidence_per_mille: 400,
            timestamp: 20,
        }];
        let merged = merge_queue(&server, &local);
        assert_eq!(merged.len(), 1, "same content_id dedupes to one row");
        assert_eq!(
            merged[0].action,
            Some(7),
            "the server row wins the collision"
        );
    }

    /// The appeal affordance is offered per ROW KIND, not per page: a
    /// nest-issued enforcement row carries it, a local detection does not —
    /// the shared `row_is_appealable` rule, painted
    /// (`moderation.md` § Legal takedown, the transparency triple).
    #[test]
    fn only_a_server_row_paints_the_appeal_button() {
        let mut app = crate::app::tests::test_app();
        app.moderation.rows = vec![row("illegal", Some(7), 1000), row("spam", None, 500)];
        let els = queue_elements(&app);
        assert_eq!(
            els.iter().filter(|e| e.id == "appeal-button").count(),
            1,
            "one appeal handle for the one decision in the queue",
        );
    }

    /// Opening an appeal paints the form under its own row, and the submit
    /// control starts DISABLED with the missing-reason stated — the shared
    /// fold's guard, rendered, so a reasonless appeal never reaches the wire.
    #[test]
    fn the_open_appeal_form_blocks_until_a_reason_is_typed() {
        let mut app = crate::app::tests::test_app();
        app.moderation.rows = vec![row("illegal", Some(7), 1000)];
        assert!(apply_local(&mut app, Action::OpenAppeal { index: 0 }).is_none());

        let els = queue_elements(&app);
        let ids: Vec<&str> = els.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "page-heading",
                "moderation-queue",
                "content-label-badge",
                "",
                "",
                "train-correction-button",
                "appeal-button",
                // The form, under the row it is against.
                "",
                "appeal-reason-input",
                "appeal-submit-button",
                "appeal-cancel-button",
                "",
            ],
        );
        let submit = els
            .iter()
            .find(|e| e.id == "appeal-submit-button")
            .expect("the submit control paints");
        assert!(
            !submit.enabled,
            "a reasonless appeal is never submittable — the wire refuses it",
        );
        assert_eq!(
            els.last().expect("the blocked reason").text,
            fauna_i18n::strings::moderation::APPEAL_BLOCKED_NO_REASON,
            "the disabled control states why",
        );

        // A dispatch attempted anyway is refused locally — belt to the paint.
        assert!(apply_local(&mut app, Action::SubmitAppeal).is_none());
    }

    /// With a reason typed the submit control goes live, and the draft still
    /// names the row it was opened against **after the queue reorders under
    /// it** — the content id is captured at OPEN time (what `Op::Appeal`
    /// carries), so a refetch cannot redirect an appeal already in progress.
    ///
    /// The dispatch itself is not built here: like every other tui page, these
    /// unit tests cover the decisions and leave the `NestClient` half to the
    /// e2e journey (`tests/e2e-unified/tests/test_moderation_appeal.py`).
    #[test]
    fn a_reasoned_appeal_keeps_its_own_row_when_the_queue_reorders() {
        let mut app = crate::app::tests::test_app();
        let target = "ab".repeat(32);
        app.moderation.rows = vec![row("illegal", Some(7), 1000)];
        apply_local(&mut app, Action::OpenAppeal { index: 0 });
        assert!(
            app.set_field(
                Field::Moderation(ModerationField::AppealReason),
                " I hold the licence ".into(),
            )
            .is_none(),
            "typing an appeal reason queues no nest write",
        );

        let els = queue_elements(&app);
        assert!(
            els.iter()
                .find(|e| e.id == "appeal-submit-button")
                .expect("submit")
                .enabled,
            "a reasoned appeal is submittable",
        );

        // The queue reorders under the open draft — a different row is now at
        // index 0, but the draft is keyed by the id it captured, not the index.
        let mut other = row("spam", Some(3), 400);
        other.content_id = "cd".repeat(32);
        app.moderation.rows.insert(0, other);

        let draft = app.moderation.appeal.as_ref().expect("the draft survives");
        assert_eq!(
            draft.content_id, target,
            "the appeal still names the decision it was opened against",
        );
        assert_eq!(draft.reason, " I hold the licence ", "stored as typed");
    }

    /// A recorded appeal closes the form and says so on `appeal-status` — the
    /// wording is *recorded for review*, never granted. The queue row STAYS
    /// (appeals are additive history, § Persistence), so the page does not
    /// pretend the decision is gone.
    #[test]
    fn a_recorded_appeal_closes_the_form_and_keeps_the_row() {
        let mut app = crate::app::tests::test_app();
        app.moderation.rows = vec![row("illegal", Some(7), 1000)];
        apply_local(&mut app, Action::OpenAppeal { index: 0 });
        apply_outcome(&mut app, Outcome::Appealed(None));

        assert!(app.moderation.appeal.is_none(), "the form closes");
        let els = queue_elements(&app);
        assert!(
            els.iter().all(|e| e.id != "appeal-reason-input"),
            "the form is gone",
        );
        assert_eq!(
            els.iter()
                .find(|e| e.id == "appeal-status")
                .expect("the outcome line")
                .text,
            fauna_i18n::strings::moderation::APPEAL_RECORDED,
        );
        assert!(
            els.iter().any(|e| e.id == "appeal-button"),
            "the decision is still in the queue, still appealable",
        );
        assert!(
            !app.errors.contains_key(&Page::Moderation),
            "a recorded appeal is not a page error",
        );
    }

    /// A REFUSED appeal — the nest's `not_found` for content it holds no
    /// enforcement record for — keeps the form open so the user can fix and
    /// resend, and lands on `appeal-status`, never on the page-level
    /// `error-message` that would blank the queue they are reading.
    #[test]
    fn a_refused_appeal_keeps_the_form_open_and_never_blanks_the_page() {
        let mut app = crate::app::tests::test_app();
        app.moderation.rows = vec![row("illegal", Some(7), 1000)];
        apply_local(&mut app, Action::OpenAppeal { index: 0 });
        apply_outcome(
            &mut app,
            Outcome::Appealed(Some("fauna.moderation.not_found".into())),
        );

        assert!(app.moderation.appeal.is_some(), "the draft survives");
        let els = queue_elements(&app);
        assert!(els.iter().any(|e| e.id == "appeal-reason-input"));
        assert!(
            els.iter()
                .find(|e| e.id == "appeal-status")
                .expect("the outcome line")
                .text
                .contains("not_found"),
            "the refusal reaches the user",
        );
        assert!(
            !app.errors.contains_key(&Page::Moderation),
            "form feedback, not a page error",
        );
    }

    /// Cancel drops the draft and the stale status line together — reopening
    /// starts clean rather than showing the last attempt's verdict.
    #[test]
    fn cancel_drops_the_draft_and_the_stale_status() {
        let mut app = crate::app::tests::test_app();
        app.moderation.rows = vec![row("illegal", Some(7), 1000)];
        apply_local(&mut app, Action::OpenAppeal { index: 0 });
        apply_outcome(&mut app, Outcome::Appealed(Some("boom".into())));
        apply_local(&mut app, Action::CancelAppeal);

        assert!(app.moderation.appeal.is_none());
        let els = queue_elements(&app);
        assert!(els.iter().all(|e| e.id != "appeal-reason-input"));
        assert!(els.iter().all(|e| e.id != "appeal-status"));
    }

    /// The offline gate reads the gesture's wire kind: the dispatch is
    /// online-only, opening and cancelling the form are not.
    #[test]
    fn only_the_dispatch_is_online_only() {
        assert_eq!(
            Action::SubmitAppeal.wire_kind(),
            Some("fauna.moderation.appeal")
        );
        assert_eq!(Action::OpenAppeal { index: 0 }.wire_kind(), None);
        assert_eq!(Action::CancelAppeal.wire_kind(), None);
    }
}
