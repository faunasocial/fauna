//! The shared **report sheet** (`report-sheet` — `moderation.md` § User-initiated
//! reporting → *App surface*), opened by the three entry verbs:
//! `feed-post-report-button` (feed ⋯), `dm-message-report-button` (message ⋯)
//! and `profile-report-button` (an OTHER profile). One module, not three
//! copies, because the sheet is one component in ui.yaml (`used_in: [feed,
//! conversations.conversation_detail, profile]`) and every decision inside it
//! is the shared `fauna_client_moderation::report` fold — the reason list, the
//! note bound, whether the include-text checkbox renders, the acknowledgement.
//! This file is state + paint + the dispatch sequence, nothing else.
//!
//! **Painted flat, once, after the opening page's own elements** (the feed
//! reply dialog's placement — [`App::elements`] appends it), so the e2e reads
//! its members unscoped: at most one sheet is ever open.
//!
//! **What a submit does, in order** (`moderation.md` § What the reporter is
//! told, § Corollary):
//! 1. `fauna.moderation.abuse_report.submit` — the report itself. A failure
//!    stops here, on the page's `error-message`; the sheet stays open so the
//!    reporter can retry.
//! 2. When `report-block-author-checkbox` was ticked, the existing
//!    `fauna.knocks.block` on the author — one gesture, two existing
//!    mechanisms; the nest only records the wish.
//! 3. The reporter-side hide: the subject's id joins the sealed
//!    `hidden_content` list on whichever preference rail is live, and the
//!    stored list the write returns replaces the render engine's copy — so
//!    the reported item paints "You reported this" on the next frame.
//!
//! Steps 2–3 failing does not un-send the report: the acknowledgement still
//! paints (the report DID go), and the follow-up failure rides
//! `error-message` beside it.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_moderation::{ModerationClient, ReportForm, ReportTarget, report};
use fauna_protocol::moderation::{AbuseReportReason, AbuseReportSubmitRequest};
use fauna_sync_engine::account_runtime::SeatAccountStore;
use fauna_ui_ids as ids;

use crate::app::{App, PageOp};
use crate::element::{Element, Field, Gesture, SelectTarget};
use crate::pages::Page;
use crate::wizard::localized;

/// The sheet's state, hung off [`App`] — one open sheet at most, app-wide.
#[derive(Default)]
pub struct ReportState {
    /// The live client, installed at the post-auth hook. `None` pre-login.
    nest: Option<Arc<NestClient>>,
    /// The open sheet, if any.
    pub(crate) sheet: Option<Sheet>,
    /// The last submit's acknowledgement (`report-status`), with the page it
    /// was sent from — it paints there until the next sheet opens or the
    /// user leaves the page.
    status: Option<(Page, String)>,
}

/// One open report sheet.
pub(crate) struct Sheet {
    /// The page the sheet opened over — where it paints and where a failure
    /// lands (`error-message` is per page).
    page: Page,
    target: ReportTarget,
    form: ReportForm,
    /// A submit is in flight — the submit control greys until it resolves.
    sending: bool,
}

impl ReportState {
    /// Whether a sheet is open over `page`.
    #[cfg(test)]
    pub(crate) fn is_open_on(&self, page: Page) -> bool {
        self.sheet.as_ref().is_some_and(|s| s.page == page)
    }
}

/// Install the live client (the post-auth hook). Everything else resets.
pub fn init(nest: Arc<NestClient>) -> ReportState {
    ReportState {
        nest: Some(nest),
        ..Default::default()
    }
}

/// The sheet's editable fields (`crate::element::Field::Report`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReportField {
    /// `report-note-input`.
    Note,
}

pub fn field(state: &ReportState, f: &ReportField) -> String {
    match f {
        ReportField::Note => state
            .sheet
            .as_ref()
            .map(|s| s.form.note.clone())
            .unwrap_or_default(),
    }
}

/// A keystroke with no open sheet is dropped — the input exists only while
/// the sheet does.
pub fn set_field(state: &mut ReportState, f: ReportField, value: String) {
    match f {
        ReportField::Note => {
            if let Some(sheet) = state.sheet.as_mut() {
                sheet.form.note = value;
            }
        }
    }
}

/// The sheet's gestures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// One of the three entry verbs — opens the sheet over the current page
    /// with the subject the verb resolved from the snapshot it painted.
    Open(Box<ReportTarget>),
    /// `report-reason-select` — the reason **token** (`spam`, `harassment`, …).
    SetReason(String),
    /// `report-include-text-checkbox`.
    ToggleIncludeText,
    /// `report-block-author-checkbox`.
    ToggleBlockAuthor,
    /// `report-submit-button`.
    Submit,
    /// `report-cancel-button` — close without sending.
    Cancel,
}

impl Action {
    /// The offline gate's input: only the submit reaches the nest.
    pub fn wire_kind(&self) -> Option<&'static str> {
        match self {
            Action::Submit => Some("fauna.moderation.abuse_report.submit"),
            Action::Open(_)
            | Action::SetReason(_)
            | Action::ToggleIncludeText
            | Action::ToggleBlockAuthor
            | Action::Cancel => None,
        }
    }
}

/// The local half of a sheet gesture → its network half.
pub fn apply_local(app: &mut App, action: Action) -> Option<Op> {
    match action {
        Action::Open(target) => {
            // The verb lives in an overflow menu (the feed ⋯ / the message ⋯);
            // the sheet replaces it, as every other one-hop verb there does.
            app.feed.actions_open = None;
            app.conversations.actions_overlay = None;
            app.report.status = None;
            app.report.sheet = Some(Sheet {
                page: app.page,
                target: *target,
                form: ReportForm::default(),
                sending: false,
            });
            None
        }
        Action::SetReason(token) => {
            let sheet = app.report.sheet.as_mut()?;
            // Only one of the eight tokens selects; anything else leaves the
            // choice unmade rather than filing under a reason nobody picked.
            sheet.form.reason = AbuseReportReason::ALL
                .into_iter()
                .find(|r| r.token() == token);
            None
        }
        Action::ToggleIncludeText => {
            let sheet = app.report.sheet.as_mut()?;
            sheet.form.include_text = !sheet.form.include_text;
            None
        }
        Action::ToggleBlockAuthor => {
            let sheet = app.report.sheet.as_mut()?;
            sheet.form.block_author = !sheet.form.block_author;
            None
        }
        Action::Cancel => {
            app.report.sheet = None;
            None
        }
        Action::Submit => {
            let nest = app.report.nest.clone()?;
            let store = app.settings.preference_store();
            let sheet = app.report.sheet.as_mut()?;
            if sheet.sending {
                return None;
            }
            // The shared fold is the gate — `None` exactly when the view said
            // the send is blocked, so a stale click cannot file a reason-less
            // report.
            let request = report::report_request(&sheet.target, &sheet.form)?;
            sheet.sending = true;
            let block = sheet
                .form
                .block_author
                .then(|| sheet.target.author.clone())
                .flatten();
            Some(Op::Submit {
                nest,
                store,
                page: sheet.page,
                request: Box::new(request),
                block,
            })
        }
    }
}

/// The network half.
pub enum Op {
    Submit {
        nest: Arc<NestClient>,
        /// The account store the hide lands on, waited for when the report
        /// is filed before the runtime is up.
        store: SeatAccountStore,
        page: Page,
        request: Box<AbuseReportSubmitRequest>,
        /// The author to block, when the reporter ticked the checkbox.
        block: Option<String>,
    },
    /// The login-time (and store-change) read of the hide list.
    LoadHidden { store: SeatAccountStore },
}

/// What an [`Op`] resolved to.
pub enum Outcome {
    /// The report landed. `hidden` is the stored hide list after step 3, when
    /// it ran; `followup_error` a step-2/3 failure to surface beside the
    /// acknowledgement.
    Submitted {
        page: Page,
        routed_to: Vec<String>,
        blocked: Option<String>,
        hidden: Option<Vec<String>>,
        followup_error: Option<String>,
    },
    /// The submit itself failed.
    Failed {
        page: Page,
        error: String,
    },
    HiddenLoaded(Vec<String>),
    /// A hide-list read failed — logged, not surfaced (the user did not ask
    /// for this read; the render simply lacks the reporter-side hide).
    HiddenFailed(String),
}

impl std::fmt::Debug for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Outcome::Submitted {
                routed_to,
                followup_error,
                ..
            } => f
                .debug_struct("Submitted")
                .field("routed_to", routed_to)
                .field("followup_error", followup_error)
                .finish(),
            Outcome::Failed { error, .. } => f.debug_tuple("Failed").field(error).finish(),
            Outcome::HiddenLoaded(ids) => f.debug_tuple("HiddenLoaded").field(&ids.len()).finish(),
            Outcome::HiddenFailed(e) => f.debug_tuple("HiddenFailed").field(e).finish(),
        }
    }
}

impl Op {
    pub async fn run(self) -> Outcome {
        match self {
            Op::Submit {
                nest,
                store,
                page,
                request,
                block,
            } => {
                let hide_id = request.subject.id().to_string();
                let reply = match ModerationClient::new(Arc::clone(&nest))
                    .abuse_report_submit(*request)
                    .await
                {
                    Ok(reply) => reply,
                    Err(e) => {
                        return Outcome::Failed {
                            page,
                            error: localized(&report::report_failed(e.to_string())),
                        };
                    }
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
                // Never a silent no-op: when the report went and the hide did
                // not, the page says so.
                let hidden =
                    match fauna_sync_engine::preference_surfaces::hide_reported(&store, &hide_id)
                        .await
                    {
                        Ok(list) => Some(list),
                        Err(e) => {
                            followup_error.get_or_insert(format!("hide: {e}"));
                            None
                        }
                    };
                Outcome::Submitted {
                    page,
                    routed_to: reply.routed_to,
                    blocked,
                    hidden,
                    followup_error,
                }
            }
            Op::LoadHidden { store } => {
                match fauna_sync_engine::preference_surfaces::load_hidden_content(&store).await {
                    Ok(list) => Outcome::HiddenLoaded(list),
                    Err(e) => Outcome::HiddenFailed(
                        fauna_sync_engine::preference_surfaces::plane_failure(e),
                    ),
                }
            }
        }
    }
}

/// Fold an [`Outcome`] back into the app.
pub fn apply_outcome(app: &mut App, outcome: Outcome) {
    match outcome {
        Outcome::Submitted {
            page,
            routed_to,
            blocked,
            hidden,
            followup_error,
        } => {
            app.report.sheet = None;
            app.report.status =
                Some((page, localized(&report::report_acknowledgement(&routed_to))));
            if let Some(list) = hidden {
                app.content_policy.set_hidden_content(list);
            }
            if let Some(actor) = blocked {
                crate::profile::note_blocked_by_report(&mut app.profile, &actor);
            }
            match followup_error {
                Some(e) => {
                    app.errors.insert(page, e);
                }
                None => {
                    app.errors.remove(&page);
                }
            }
        }
        Outcome::Failed { page, error } => {
            if let Some(sheet) = app.report.sheet.as_mut() {
                sheet.sending = false;
            }
            app.errors.insert(page, error);
        }
        Outcome::HiddenLoaded(list) => app.content_policy.set_hidden_content(list),
        Outcome::HiddenFailed(e) => {
            tracing::warn!("hidden_content read failed; the reporter-side hide is inactive: {e}");
        }
    }
}

/// The hide-list read, or `None` pre-login.
pub fn load_hidden_op(app: &App) -> Option<Op> {
    app.report.nest.as_ref()?;
    Some(Op::LoadHidden {
        store: app.settings.preference_store(),
    })
}

/// Fire the hide-list read — at the post-auth hook and whenever the account
/// store reports a sibling commit (another device may have hidden something).
/// A render source, like the spam thresholds: fetched eagerly, never on a nav
/// edge, so the first feed paint already honours it.
pub fn spawn_load_hidden(app: &App) {
    let Some(op) = load_hidden_op(app) else {
        return;
    };
    let tx = app.tx.clone();
    let generation = app.session_generation;
    tokio::spawn(async move {
        let outcome = op.run().await;
        let _ = tx.send(crate::app::UiMessage::Data(crate::app::DataMessage::Page(
            generation,
            crate::app::PageOutcome::Report(outcome),
        )));
    });
}

/// The sheet (and the last acknowledgement) for the current page — appended
/// after the page's own elements by [`App::elements`].
pub fn elements(app: &App) -> Vec<Element> {
    let mut out = Vec::new();
    if let Some((page, status)) = app.report.status.as_ref()
        && *page == app.page
    {
        out.push(Element::label(ids::REPORT_STATUS, status.clone()));
    }
    let Some(sheet) = app.report.sheet.as_ref().filter(|s| s.page == app.page) else {
        return out;
    };
    let view = report::report_sheet_view(&sheet.target.subject, sheet.target.sealed, &sheet.form);
    out.push(Element::label(ids::REPORT_SHEET, localized(&view.title)));
    let selected = sheet.form.reason.map(|r| r.token()).unwrap_or_default();
    let selected_label = view
        .reasons
        .iter()
        .find(|o| Some(o.reason) == sheet.form.reason)
        .map(|o| localized(&o.label))
        .unwrap_or_default();
    out.push(
        Element::select(
            ids::REPORT_REASON_SELECT,
            selected,
            SelectTarget::ReportReason,
            view.reasons
                .iter()
                .map(|o| o.reason.token().to_string())
                .collect(),
        )
        .display_value(selected_label)
        .labelled(localized(&view.reason_label)),
    );
    out.push(
        Element::input(
            ids::REPORT_NOTE_INPUT,
            sheet.form.note.clone(),
            Field::Report(ReportField::Note),
        )
        .labelled(localized(&view.note_label)),
    );
    if view.show_include_text {
        out.push(
            Element::checkbox_gesture(
                ids::REPORT_INCLUDE_TEXT_CHECKBOX,
                localized(&view.include_text_label),
                sheet.form.include_text,
                Gesture::Report(Action::ToggleIncludeText),
            )
            .attr("state", if sheet.form.include_text { "on" } else { "off" }),
        );
    }
    out.push(
        Element::checkbox_gesture(
            ids::REPORT_BLOCK_AUTHOR_CHECKBOX,
            localized(&view.block_author_label),
            sheet.form.block_author,
            Gesture::Report(Action::ToggleBlockAuthor),
        )
        .attr("state", if sheet.form.block_author { "on" } else { "off" }),
    );
    out.push(Element::gesture_button(
        ids::REPORT_SUBMIT_BUTTON,
        localized(&view.submit_label),
        view.can_submit && !sheet.sending,
        Gesture::Report(Action::Submit),
    ));
    if let Some(reason) = view.blocked_reason.as_ref() {
        // The disabled submit owes its reason — form guidance, as chrome.
        out.push(Element::chrome(localized(reason)));
    }
    out.push(Element::gesture_button(
        ids::REPORT_CANCEL_BUTTON,
        localized(&view.cancel_label),
        true,
        Gesture::Report(Action::Cancel),
    ));
    out
}

/// The report target for a conversation message, off the snapshot the
/// thread paints: its plane identity (the shared `message_subject` parse), the
/// sender's actor id for routing and the account-level hide, and the text the
/// reporter may choose to attach. `None` for a message with no plane
/// identity (mail and bridged rails) — there is nothing to report it against.
pub fn message_target(msg: &fauna_conversations::MessageSnapshot) -> Option<ReportTarget> {
    let plane = msg.plane_ref.as_ref()?;
    ReportTarget::message(
        &plane.scope,
        &plane.record_digest,
        msg.sender.person_actor_id().map(|a| a.to_hex()),
        &msg.document.to_plaintext(),
    )
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

/// Clear the page-scoped acknowledgement when the user leaves its page — the
/// nav edge (`App::apply`) calls this.
pub fn on_nav(state: &mut ReportState, to: Page) {
    if state.status.as_ref().is_some_and(|(page, _)| *page != to) {
        state.status = None;
    }
    if state.sheet.as_ref().is_some_and(|s| s.page != to) {
        state.sheet = None;
    }
}

/// The PageOp wrapper for the one gesture door.
pub fn gesture(app: &mut App, action: Action) -> Option<PageOp> {
    apply_local(app, action).map(PageOp::Report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids_of(els: &[Element]) -> Vec<&str> {
        els.iter().map(|e| e.id.as_str()).collect()
    }

    fn app_on(page: Page) -> App {
        let mut app = crate::app::tests::test_app();
        app.page = page;
        app
    }

    fn open(app: &mut App, target: ReportTarget) {
        assert!(apply_local(app, Action::Open(Box::new(target))).is_none());
    }

    fn message() -> ReportTarget {
        ReportTarget {
            subject: fauna_protocol::moderation::AbuseReportSubject::Message {
                channel: "cd".repeat(32),
                record_cid: "0e".repeat(32),
            },
            sealed: true,
            author: Some("ef".repeat(32)),
            plaintext: Some("the words".into()),
        }
    }

    /// The sheet paints its ui.yaml members once opened, the include-text
    /// checkbox only for a sealed subject — the shared fold's decision 2.
    #[test]
    fn the_sheet_paints_its_members_and_the_sealed_checkbox_only_when_sealed() {
        let mut app = app_on(Page::Profile);
        assert!(
            elements(&app).is_empty(),
            "nothing paints before a verb opens it"
        );
        open(&mut app, actor_target(&"ab".repeat(32)));
        let els = elements(&app);
        assert_eq!(
            ids_of(&els),
            vec![
                "report-sheet",
                "report-reason-select",
                "report-note-input",
                "report-block-author-checkbox",
                "report-submit-button",
                "",
                "report-cancel-button",
            ],
            "an account report has no text to attach"
        );

        let mut app = app_on(Page::Conversations);
        open(&mut app, message());
        let ids = ids_of(&elements(&app)).join(",");
        assert!(ids.contains("report-include-text-checkbox"), "{ids}");
    }

    /// The submit stays disabled, with its reason stated, until a reason is
    /// picked — and only one of the eight tokens picks one.
    #[test]
    fn submit_waits_for_a_real_reason() {
        let mut app = app_on(Page::Feed);
        open(&mut app, message());
        let submit = |app: &App| {
            elements(app)
                .into_iter()
                .find(|e| e.id == "report-submit-button")
                .expect("submit paints")
                .enabled
        };
        assert!(!submit(&app));
        assert!(
            apply_local(&mut app, Action::Submit).is_none(),
            "a reasonless submit never dispatches"
        );
        let _ = apply_local(&mut app, Action::SetReason("not-a-reason".into()));
        assert!(!submit(&app));
        let _ = apply_local(&mut app, Action::SetReason("harassment".into()));
        assert!(submit(&app));
        let select = elements(&app)
            .into_iter()
            .find(|e| e.id == "report-reason-select")
            .expect("select paints");
        assert_eq!(
            select.text, "harassment",
            "the select round-trips the token"
        );
    }

    /// The sheet belongs to the page it opened over: another page never paints
    /// it, cancel closes it, and leaving the page closes it too.
    #[test]
    fn the_sheet_lives_and_dies_with_its_page() {
        let mut app = app_on(Page::Feed);
        open(&mut app, message());
        assert!(app.report.is_open_on(Page::Feed));
        app.page = Page::Contacts;
        assert!(elements(&app).is_empty());
        on_nav(&mut app.report, Page::Contacts);
        assert!(app.report.sheet.is_none());

        let mut app = app_on(Page::Feed);
        open(&mut app, message());
        let _ = apply_local(&mut app, Action::Cancel);
        assert!(app.report.sheet.is_none());
    }

    /// A submit that lands closes the sheet, paints the acknowledgement naming
    /// the destinations, and hands the stored hide list to the render engine;
    /// a failed one keeps the sheet for a retry.
    #[test]
    fn a_landed_report_acknowledges_and_hides() {
        let mut app = app_on(Page::Feed);
        open(&mut app, message());
        apply_outcome(
            &mut app,
            Outcome::Failed {
                page: Page::Feed,
                error: "boom".into(),
            },
        );
        assert!(app.report.sheet.is_some(), "a failed send keeps the sheet");
        assert_eq!(
            app.errors.get(&Page::Feed).map(String::as_str),
            Some("boom")
        );

        apply_outcome(
            &mut app,
            Outcome::Submitted {
                page: Page::Feed,
                routed_to: vec!["a.example".into(), "b.example".into()],
                blocked: None,
                hidden: Some(vec!["0e".repeat(32)]),
                followup_error: None,
            },
        );
        assert!(app.report.sheet.is_none());
        let status = elements(&app)
            .into_iter()
            .find(|e| e.id == "report-status")
            .expect("the acknowledgement paints");
        assert!(
            status.text.contains("a.example") && status.text.contains("b.example"),
            "{}",
            status.text
        );
        assert!(!app.errors.contains_key(&Page::Feed));
        assert!(
            app.content_policy
                .verdict_for_item(&"0e".repeat(32), None, &[])
                .reported(),
            "the reported message is hidden for the reporter"
        );
    }

    /// Only the submit reaches the nest — the offline gate greys nothing else.
    #[test]
    fn only_submit_carries_a_wire_kind() {
        assert_eq!(
            Action::Submit.wire_kind(),
            Some("fauna.moderation.abuse_report.submit")
        );
        assert_eq!(Action::Cancel.wire_kind(), None);
        assert_eq!(Action::Open(Box::new(message())).wire_kind(), None);
    }
}
