//! User-initiated reporting's shared decision logic (`moderation.md`
//! § User-initiated reporting → *Where logic lives*) — the twin of the
//! takedown console's [`crate::takedown`] module. Every sentence the report
//! sheet, the reporter's ledger and the admin queue paint is picked here, so
//! the six-app trickle-down after tui is paint-only (priority #2).
//!
//! Decisions that live here because each is one an app would get wrong alone:
//!
//! 1. **The reason vocabulary** is the shared [`AbuseReportReason`] enum,
//!    labelled through `LocalizedText` — no app hard-codes a reason string.
//! 2. **The excerpt checkbox appears only for a sealed subject**, and its label
//!    names the consequence ("the admins will be able to read this message").
//!    A public post needs no excerpt: the admin opens it by id.
//! 3. **The bounds** are the wire's own constants: an over-long note blocks the
//!    send with a stated reason; an attached excerpt is cut to the bound on a
//!    character boundary (the reporter did not type it, so refusing would be a
//!    dead end).
//! 4. **The acknowledgement names where the report went** — this nest, and the
//!    author's home nest when forwarded.
//! 5. **Ledger and queue rows** word status, outcome and origin the same way on
//!    every app; the queue never names a forwarded report's reporter.

use fauna_core::localized::LocalizedText;
use fauna_protocol::moderation::{
    AbuseReportMineEntry, AbuseReportOutcome, AbuseReportQueueEntry, AbuseReportReason,
    AbuseReportStatus, AbuseReportSubject, AbuseReportSubmitRequest,
    MAX_ABUSE_REPORT_EXCERPT_BYTES, MAX_ABUSE_REPORT_NOTE_BYTES,
};
use serde::{Deserialize, Serialize};

/// The report sheet's draft, exactly as the reporter left it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportForm {
    /// `None` until the reporter picks one — the sheet preselects nothing, so
    /// no report is filed under a reason nobody chose.
    pub reason: Option<AbuseReportReason>,
    pub note: String,
    /// The excerpt checkbox — meaningful only for a sealed subject.
    pub include_text: bool,
    pub block_author: bool,
}

/// One entry of the sheet's reason picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportReasonOption {
    pub reason: AbuseReportReason,
    pub label: LocalizedText,
}

/// What the report sheet renders, derived once per keystroke.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportSheetView {
    pub title: LocalizedText,
    pub reason_label: LocalizedText,
    pub reasons: Vec<ReportReasonOption>,
    pub note_label: LocalizedText,
    /// Whether `report-include-text-checkbox` renders at all (decision 2).
    pub show_include_text: bool,
    pub include_text_label: LocalizedText,
    pub block_author_label: LocalizedText,
    pub submit_label: LocalizedText,
    pub cancel_label: LocalizedText,
    pub can_submit: bool,
    /// Why the send is blocked, when it is.
    pub blocked_reason: Option<LocalizedText>,
}

/// The `LocalizedText` label for one reason.
pub fn reason_label(reason: AbuseReportReason) -> LocalizedText {
    LocalizedText::key(format!("moderation.report.reason.{}", reason.token()))
}

/// Fold the draft into the sheet's render decisions. `sealed` is whether the
/// nest holds no readable bytes for the subject — always for a conversation
/// message; for a post, whether the client opened it through a key (a gated
/// post). The client knows; the nest does not need to.
pub fn report_sheet_view(
    subject: &AbuseReportSubject,
    sealed: bool,
    form: &ReportForm,
) -> ReportSheetView {
    let blocked_reason = if form.reason.is_none() {
        Some(LocalizedText::key("moderation.report.blocked_no_reason"))
    } else if form.note.trim().len() > MAX_ABUSE_REPORT_NOTE_BYTES {
        Some(LocalizedText::key(
            "moderation.report.blocked_note_too_long",
        ))
    } else {
        None
    };
    ReportSheetView {
        title: LocalizedText::key("moderation.report.title"),
        reason_label: LocalizedText::key("moderation.report.reason_label"),
        reasons: AbuseReportReason::ALL
            .into_iter()
            .map(|reason| ReportReasonOption {
                reason,
                label: reason_label(reason),
            })
            .collect(),
        note_label: LocalizedText::key("moderation.report.note_label"),
        show_include_text: subject_is_sealed(subject, sealed),
        include_text_label: LocalizedText::key("moderation.report.include_text_label"),
        block_author_label: LocalizedText::key("moderation.report.block_author_label"),
        submit_label: LocalizedText::key("moderation.report.submit"),
        cancel_label: LocalizedText::key("moderation.report.cancel"),
        can_submit: blocked_reason.is_none(),
        blocked_reason,
    }
}

fn subject_is_sealed(subject: &AbuseReportSubject, sealed: bool) -> bool {
    match subject {
        AbuseReportSubject::Actor { .. } => false,
        _ => subject.is_always_sealed() || sealed,
    }
}

/// Cut `text` to at most `max` UTF-8 bytes on a character boundary.
fn truncate_to_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// What is being reported, as the surface that opened the sheet holds it:
/// a post card, a conversation message, or a profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportTarget {
    pub subject: AbuseReportSubject,
    /// Whether the nest holds no readable bytes for the subject (see
    /// [`report_sheet_view`]).
    pub sealed: bool,
    /// The subject's author (hex actor id) — routes the report to the author's
    /// home nest.
    pub author: Option<String>,
    /// The subject's text the client already holds — the excerpt source when
    /// the reporter ticks the checkbox on a sealed subject.
    pub plaintext: Option<String>,
}

/// Build the submit request from a sheet the view said may be sent. Returns
/// `None` when it may not (no reason, or an over-long note). The excerpt is
/// attached only when the subject is sealed **and** the reporter ticked the
/// checkbox, cut to the wire bound.
pub fn report_request(
    target: &ReportTarget,
    form: &ReportForm,
) -> Option<AbuseReportSubmitRequest> {
    let subject = &target.subject;
    if !report_sheet_view(subject, target.sealed, form).can_submit {
        return None;
    }
    let note = form.note.trim();
    let excerpt = (form.include_text && subject_is_sealed(subject, target.sealed))
        .then_some(target.plaintext.as_deref())
        .flatten()
        .map(|t| truncate_to_bytes(t.trim(), MAX_ABUSE_REPORT_EXCERPT_BYTES))
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    Some(AbuseReportSubmitRequest {
        subject: subject.clone(),
        reason: form.reason?,
        note: (!note.is_empty()).then(|| note.to_string()),
        excerpt,
        block_author: form.block_author,
        subject_actor: target.author.clone(),
        extra: Default::default(),
    })
}

/// The `report-status` acknowledgement after a send (decision 4): names the
/// reporter's own nest, and the author's home nest when the report was
/// forwarded. `routed_to` is the submit reply's list, own nest first.
pub fn report_acknowledgement(routed_to: &[String]) -> LocalizedText {
    match routed_to {
        [own, home, ..] => LocalizedText::key_args(
            "moderation.report.sent_forwarded",
            [("nest", own.clone()), ("home_nest", home.clone())],
        ),
        [own] => LocalizedText::key_arg("moderation.report.sent_local", "nest", own.clone()),
        [] => LocalizedText::key_arg("moderation.report.sent_local", "nest", String::new()),
    }
}

/// The failure line a send renders on the page's `error-message`.
pub fn report_failed(error: impl Into<String>) -> LocalizedText {
    LocalizedText::key_arg("moderation.report.failed", "error", error.into())
}

/// One reporter-ledger row, worded (`moderation-report-item`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerRowView {
    pub reason: LocalizedText,
    pub status: LocalizedText,
    /// The outcome of a resolved report.
    pub outcome: Option<LocalizedText>,
    pub routed_to: LocalizedText,
    /// Whether `moderation-report-withdraw-button` renders — open rows only.
    pub can_withdraw: bool,
}

fn status_label(status: AbuseReportStatus) -> LocalizedText {
    match status {
        AbuseReportStatus::Open | AbuseReportStatus::Unknown => {
            LocalizedText::key("moderation.report.status_open")
        }
        AbuseReportStatus::Resolved => LocalizedText::key("moderation.report.status_resolved"),
        AbuseReportStatus::Withdrawn => LocalizedText::key("moderation.report.status_withdrawn"),
    }
}

fn outcome_label(outcome: AbuseReportOutcome) -> Option<LocalizedText> {
    match outcome {
        AbuseReportOutcome::Acted => Some(LocalizedText::key("moderation.report.outcome_acted")),
        AbuseReportOutcome::Dismissed => {
            Some(LocalizedText::key("moderation.report.outcome_dismissed"))
        }
        AbuseReportOutcome::Unknown => None,
    }
}

pub fn ledger_row_view(entry: &AbuseReportMineEntry) -> LedgerRowView {
    LedgerRowView {
        reason: reason_label(entry.reason),
        status: status_label(entry.status),
        outcome: entry.outcome.and_then(outcome_label),
        routed_to: LocalizedText::key_arg(
            "moderation.report.ledger_routed_to",
            "destinations",
            entry.routed_to.join(", "),
        ),
        can_withdraw: entry.status == AbuseReportStatus::Open,
    }
}

/// The ledger's header and empty-state words.
pub fn ledger_title() -> LocalizedText {
    LocalizedText::key("moderation.report.ledger_title")
}

pub fn ledger_empty() -> LocalizedText {
    LocalizedText::key("moderation.report.ledger_empty")
}

/// The line a withdraw paints — success names what was deleted.
pub fn withdraw_verdict(error: Option<String>) -> LocalizedText {
    match error {
        Some(e) => report_failed(e),
        None => LocalizedText::key("moderation.report.withdrawn"),
    }
}

/// One admin-queue row's origin line (decision 5): a local reporter's handle,
/// or "a user of <nest>" for a forwarded report — never the forwarded
/// reporter's identity, which never reached this nest.
pub fn queue_origin(entry: &AbuseReportQueueEntry) -> LocalizedText {
    match (&entry.reporter_handle, &entry.origin_nest) {
        (Some(handle), _) => LocalizedText::key_arg(
            "admin.nest_page.reports_origin_local",
            "handle",
            handle.clone(),
        ),
        (None, Some(nest)) => LocalizedText::key_arg(
            "admin.nest_page.reports_origin_forwarded",
            "nest",
            nest.clone(),
        ),
        (None, None) => LocalizedText::key_arg(
            "admin.nest_page.reports_origin_forwarded",
            "nest",
            String::new(),
        ),
    }
}

/// The report subject for a conversation message, from the account-plane
/// identity the message snapshot already carries
/// (`fauna_conversations::MessageSnapshot::plane_ref` — its
/// `content:conv:<channel-hex>` scope and its record digest). The digest is
/// the same 32-byte hex the legal-takedown console's conversation half takes,
/// so a queue row's *open takedown* pre-fill needs no second lookup. `None`
/// for anything that is not a conversation record (a mail-rail message has no
/// plane identity, and nothing on this nest to report it against).
pub fn message_subject(plane_scope: &str, record_digest: &str) -> Option<AbuseReportSubject> {
    let fauna_protocol::scope::Scope::Content(scope) = plane_scope.parse().ok()? else {
        return None;
    };
    if scope.kind() != "conv" || !fauna_core::hex32::is_lowercase_hex64(record_digest) {
        return None;
    }
    Some(AbuseReportSubject::Message {
        channel: fauna_core::hex32::encode(scope.scope_id()),
        record_cid: record_digest.to_string(),
    })
}

/// What *open takedown* pre-fills the legal-takedown console with for one
/// queue row's subject (`moderation.md` § Where it lands → the admin's
/// levers): a post's cid as a post takedown, a message's record digest as a
/// conversation takedown. An account has no takedown — its lever is the
/// `admin-users` suspension, reached by navigation — so it pre-fills nothing.
/// The console's own guards still apply: the pre-fill carries no citation, so
/// the arm stays disabled until the admin types one.
pub fn takedown_prefill(subject: &AbuseReportSubject) -> Option<crate::takedown::TakedownForm> {
    let content_type = match subject {
        AbuseReportSubject::Post { .. } => crate::takedown::TakedownContentType::Post,
        AbuseReportSubject::Message { .. } => crate::takedown::TakedownContentType::Conversation,
        AbuseReportSubject::Actor { .. } | AbuseReportSubject::Unknown(_) => return None,
    };
    Some(crate::takedown::TakedownForm {
        content_id: subject.id().to_string(),
        content_type,
        ..Default::default()
    })
}

/// One admin-queue row, worded (`admin-nest-report-item`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueRowView {
    pub reason: LocalizedText,
    /// Decision 5's origin line.
    pub origin: LocalizedText,
    /// The subject's stored kind token (`post` / `message` / `actor`).
    pub subject_kind: String,
    pub subject_id: String,
    pub note: Option<String>,
    /// What the reporter chose to attach — the only readable bytes of a
    /// sealed subject an admin ever sees.
    pub excerpt: Option<String>,
    /// Whether `admin-nest-report-open-takedown-button` renders — posts and
    /// messages only ([`takedown_prefill`]).
    pub can_open_takedown: bool,
}

pub fn queue_row_view(entry: &AbuseReportQueueEntry) -> QueueRowView {
    QueueRowView {
        reason: reason_label(entry.reason),
        origin: queue_origin(entry),
        subject_kind: entry.subject.kind().to_string(),
        subject_id: entry.subject.id().to_string(),
        note: entry.note.clone(),
        excerpt: entry.excerpt.clone(),
        can_open_takedown: takedown_prefill(&entry.subject).is_some(),
    }
}

/// The line the admin queue paints after resolving a row.
pub fn resolve_verdict(outcome: AbuseReportOutcome, error: Option<String>) -> LocalizedText {
    match (error, outcome) {
        (Some(e), _) => LocalizedText::key_arg("admin.nest_page.reports_failed", "error", e),
        (None, AbuseReportOutcome::Dismissed) => {
            LocalizedText::key("admin.nest_page.reports_resolved_dismissed")
        }
        (None, _) => LocalizedText::key("admin.nest_page.reports_resolved_acted"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post() -> AbuseReportSubject {
        AbuseReportSubject::Post {
            cid: "ab".repeat(32),
        }
    }

    fn message() -> AbuseReportSubject {
        AbuseReportSubject::Message {
            channel: "cd".repeat(16),
            record_cid: "bafyrecord".into(),
        }
    }

    fn build(
        subject: AbuseReportSubject,
        sealed: bool,
        form: &ReportForm,
        plaintext: Option<&str>,
    ) -> Option<AbuseReportSubmitRequest> {
        let target = ReportTarget {
            subject,
            sealed,
            author: Some("ef".repeat(32)),
            plaintext: plaintext.map(str::to_string),
        };
        let req = report_request(&target, form)?;
        // The author always rides along — it is what routes the report.
        assert_eq!(req.subject_actor.as_deref(), Some("ef".repeat(32).as_str()));
        Some(req)
    }

    fn form(reason: Option<AbuseReportReason>, note: &str, include_text: bool) -> ReportForm {
        ReportForm {
            reason,
            note: note.into(),
            include_text,
            block_author: false,
        }
    }

    #[test]
    fn no_reason_blocks_the_send_with_a_stated_reason() {
        let view = report_sheet_view(&post(), false, &form(None, "", false));
        assert!(!view.can_submit);
        assert_eq!(
            view.blocked_reason.unwrap().key,
            "moderation.report.blocked_no_reason"
        );
        assert!(build(post(), false, &form(None, "", false), None).is_none());
    }

    #[test]
    fn an_over_long_note_blocks_the_send() {
        let long = "x".repeat(MAX_ABUSE_REPORT_NOTE_BYTES + 1);
        let f = form(Some(AbuseReportReason::Spam), &long, false);
        let view = report_sheet_view(&post(), false, &f);
        assert!(!view.can_submit);
        assert_eq!(
            view.blocked_reason.unwrap().key,
            "moderation.report.blocked_note_too_long"
        );
        let at_bound = "x".repeat(MAX_ABUSE_REPORT_NOTE_BYTES);
        assert!(
            report_sheet_view(
                &post(),
                false,
                &form(Some(AbuseReportReason::Spam), &at_bound, false)
            )
            .can_submit
        );
    }

    #[test]
    fn the_reason_picker_offers_all_eight_reasons_through_localized_labels() {
        let view = report_sheet_view(&post(), false, &ReportForm::default());
        assert_eq!(view.reasons.len(), 8);
        assert_eq!(view.reasons[0].label.key, "moderation.report.reason.spam");
        assert_eq!(view.reasons[7].reason, AbuseReportReason::Other);
        assert_eq!(view.reasons[7].label.key, "moderation.report.reason.other");
    }

    /// Decision 2: the excerpt checkbox renders only for a sealed subject.
    #[test]
    fn the_include_text_checkbox_renders_only_for_a_sealed_subject() {
        let f = ReportForm::default();
        assert!(report_sheet_view(&message(), false, &f).show_include_text);
        assert!(report_sheet_view(&post(), true, &f).show_include_text);
        assert!(!report_sheet_view(&post(), false, &f).show_include_text);
        let actor = AbuseReportSubject::Actor {
            actor_id: "ef".repeat(32),
        };
        assert!(!report_sheet_view(&actor, true, &f).show_include_text);
    }

    #[test]
    fn the_excerpt_is_attached_only_when_sealed_and_ticked() {
        let reason = Some(AbuseReportReason::Harassment);
        let ticked = form(reason, "", true);
        let req = build(message(), false, &ticked, Some(" the text ")).unwrap();
        assert_eq!(req.excerpt.as_deref(), Some("the text"));
        // Unticked: nothing leaves the device.
        let req = build(message(), false, &form(reason, "", false), Some("t")).unwrap();
        assert!(req.excerpt.is_none());
        // Ticked on a public post: the checkbox never rendered, so nothing is attached.
        let req = build(post(), false, &ticked, Some("t")).unwrap();
        assert!(req.excerpt.is_none());
    }

    #[test]
    fn an_excerpt_is_cut_to_the_bound_on_a_char_boundary() {
        let text = "é".repeat(MAX_ABUSE_REPORT_EXCERPT_BYTES); // 2 bytes each
        let f = form(Some(AbuseReportReason::Other), "", true);
        let req = build(message(), false, &f, Some(&text)).unwrap();
        let excerpt = req.excerpt.unwrap();
        assert!(excerpt.len() <= MAX_ABUSE_REPORT_EXCERPT_BYTES);
        assert_eq!(excerpt.len(), MAX_ABUSE_REPORT_EXCERPT_BYTES);
        assert!(excerpt.chars().all(|c| c == 'é'));
    }

    #[test]
    fn the_note_is_trimmed_and_an_empty_note_is_omitted() {
        let reason = Some(AbuseReportReason::Spam);
        let req = build(post(), false, &form(reason, "  why  ", false), None).unwrap();
        assert_eq!(req.note.as_deref(), Some("why"));
        let req = build(post(), false, &form(reason, "   ", false), None).unwrap();
        assert!(req.note.is_none());
    }

    #[test]
    fn the_acknowledgement_names_every_destination() {
        let local = report_acknowledgement(&["a.example".into()]);
        assert_eq!(local.key, "moderation.report.sent_local");
        assert_eq!(local.args["nest"], "a.example");
        let fwd = report_acknowledgement(&["a.example".into(), "b.example".into()]);
        assert_eq!(fwd.key, "moderation.report.sent_forwarded");
        assert_eq!(fwd.args["nest"], "a.example");
        assert_eq!(fwd.args["home_nest"], "b.example");
    }

    #[test]
    fn ledger_rows_offer_withdraw_only_while_open() {
        let mut entry = AbuseReportMineEntry {
            report_id: "0f".repeat(16),
            subject: post(),
            reason: AbuseReportReason::Hate,
            created_at: 1,
            status: AbuseReportStatus::Open,
            outcome: None,
            routed_to: vec!["a.example".into(), "b.example".into()],
            extra: Default::default(),
        };
        let open = ledger_row_view(&entry);
        assert!(open.can_withdraw);
        assert_eq!(open.status.key, "moderation.report.status_open");
        assert_eq!(open.routed_to.args["destinations"], "a.example, b.example");
        assert!(open.outcome.is_none());

        entry.status = AbuseReportStatus::Resolved;
        entry.outcome = Some(AbuseReportOutcome::Dismissed);
        let resolved = ledger_row_view(&entry);
        assert!(!resolved.can_withdraw);
        assert_eq!(
            resolved.outcome.unwrap().key,
            "moderation.report.outcome_dismissed"
        );
    }

    /// Decision 5: a forwarded report's origin names the nest, never a person.
    #[test]
    fn the_queue_origin_names_a_local_handle_or_the_forwarding_nest() {
        let mut entry = AbuseReportQueueEntry {
            report_id: "0f".repeat(16),
            subject: post(),
            subject_actor: None,
            reason: AbuseReportReason::Spam,
            note: None,
            excerpt: None,
            reporter_handle: Some("alice".into()),
            origin_nest: None,
            created_at: 1,
            extra: Default::default(),
        };
        let local = queue_origin(&entry);
        assert_eq!(local.key, "admin.nest_page.reports_origin_local");
        assert_eq!(local.args["handle"], "alice");
        entry.reporter_handle = None;
        entry.origin_nest = Some("b.example".into());
        let fwd = queue_origin(&entry);
        assert_eq!(fwd.key, "admin.nest_page.reports_origin_forwarded");
        assert_eq!(fwd.args["nest"], "b.example");
    }

    /// A conversation record's plane identity is its report subject — keyed by
    /// the record digest the takedown console also takes; nothing else is.
    #[test]
    fn a_messages_plane_ref_is_its_report_subject() {
        let channel = "cd".repeat(32);
        let digest = "0e".repeat(32);
        let subject = message_subject(&format!("content:conv:{channel}"), &digest).unwrap();
        assert_eq!(
            subject,
            AbuseReportSubject::Message {
                channel: channel.clone(),
                record_cid: digest.clone(),
            }
        );
        // Another content kind, a malformed scope, or a non-digest record id
        // names no reportable message.
        assert!(message_subject(&format!("content:post:{channel}"), &digest).is_none());
        assert!(message_subject("content:conv:zz", &digest).is_none());
        assert!(message_subject(&format!("content:conv:{channel}"), "bafy").is_none());
    }

    /// Open takedown pre-fills a post or a message, never an account, and
    /// carries no citation — the console's own guard still has to be met.
    #[test]
    fn open_takedown_prefills_posts_and_messages_only() {
        let from_post = takedown_prefill(&post()).unwrap();
        assert_eq!(from_post.content_id, "ab".repeat(32));
        assert_eq!(
            from_post.content_type,
            crate::takedown::TakedownContentType::Post
        );
        assert!(from_post.legal_reference.is_empty() && !from_post.restore);
        let from_message = takedown_prefill(&message()).unwrap();
        assert_eq!(from_message.content_id, "bafyrecord");
        assert_eq!(
            from_message.content_type,
            crate::takedown::TakedownContentType::Conversation
        );
        let actor = AbuseReportSubject::Actor {
            actor_id: "ef".repeat(32),
        };
        assert!(takedown_prefill(&actor).is_none());
        assert!(!crate::takedown::takedown_form_view(&from_post).can_submit);
    }

    #[test]
    fn a_queue_row_carries_what_the_reporter_attached_and_its_levers() {
        let entry = AbuseReportQueueEntry {
            report_id: "0f".repeat(16),
            subject: message(),
            subject_actor: None,
            reason: AbuseReportReason::Harassment,
            note: Some("see this".into()),
            excerpt: Some("the words".into()),
            reporter_handle: None,
            origin_nest: Some("b.example".into()),
            created_at: 1,
            extra: Default::default(),
        };
        let row = queue_row_view(&entry);
        assert_eq!(row.reason.key, "moderation.report.reason.harassment");
        assert_eq!(row.origin.key, "admin.nest_page.reports_origin_forwarded");
        assert_eq!(row.subject_kind, "message");
        assert_eq!(row.subject_id, "bafyrecord");
        assert_eq!(row.excerpt.as_deref(), Some("the words"));
        assert!(row.can_open_takedown);
        let actor_row = queue_row_view(&AbuseReportQueueEntry {
            subject: AbuseReportSubject::Actor {
                actor_id: "ef".repeat(32),
            },
            ..entry
        });
        assert!(!actor_row.can_open_takedown);
    }

    #[test]
    fn the_resolve_verdict_words_each_outcome() {
        assert_eq!(
            resolve_verdict(AbuseReportOutcome::Acted, None).key,
            "admin.nest_page.reports_resolved_acted"
        );
        assert_eq!(
            resolve_verdict(AbuseReportOutcome::Dismissed, None).key,
            "admin.nest_page.reports_resolved_dismissed"
        );
        let failed = resolve_verdict(AbuseReportOutcome::Acted, Some("not_found".into()));
        assert_eq!(failed.key, "admin.nest_page.reports_failed");
        assert_eq!(failed.args["error"], "not_found");
    }
}
