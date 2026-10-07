//! UniFFI faces of user-initiated reporting (`moderation.md` § User-initiated
//! reporting → *Where logic lives*): the shared `fauna_client_moderation::report`
//! decisions — the sheet, the acknowledgement, the ledger and queue wording —
//! and the five client calls, so the six-app trickle-down after tui is
//! paint-only (priority #2).
//!
//! Gated `moderation-badge` for the takedown console's reason
//! (`moderation_client.rs`): the views embed bare `LocalizedText`, which
//! `uniffi-bindgen-go` cannot emit, and the Go mail-bridge has no report
//! surface.
//!
//! Reasons, statuses and outcomes cross as their wire tokens (`spam`,
//! `open`, `acted`, …): the vocabulary stays the shared enum's, and a token a
//! newer nest adds decodes to its catch-all exactly as on the wire.

use fauna_client_moderation::report::{
    self, LedgerRowView, ReportForm, ReportSheetView, ReportTarget,
};
use fauna_core::carried::CarriedValue;
use fauna_core::localized::LocalizedText;
use fauna_protocol::moderation::{
    AbuseReportMineEntry, AbuseReportOutcome, AbuseReportQueueEntry, AbuseReportReason,
    AbuseReportSubject,
};
use fauna_protocol::{Value, decode_strict, encode_canonical};

use crate::admin::FfiAdminClient;
use crate::moderation_client::FfiModerationClient;
use crate::{FfiError, general_err, stringify};

/// What is being reported (the wire's `AbuseReportSubject`).
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum FfiReportSubject {
    Post {
        cid: String,
    },
    Message {
        channel: String,
        record_cid: String,
    },
    Actor {
        actor_id: String,
    },
    /// A subject kind a newer nest recorded that this build cannot read,
    /// carried as its canonical dag-cbor. Shown neutral; it offers no action
    /// and is never submitted (the nest refuses it).
    Unknown {
        cbor: Vec<u8>,
    },
}

impl From<FfiReportSubject> for AbuseReportSubject {
    fn from(s: FfiReportSubject) -> Self {
        match s {
            FfiReportSubject::Post { cid } => AbuseReportSubject::Post { cid },
            FfiReportSubject::Message {
                channel,
                record_cid,
            } => AbuseReportSubject::Message {
                channel,
                record_cid,
            },
            FfiReportSubject::Actor { actor_id } => AbuseReportSubject::Actor { actor_id },
            // Bytes this seam never handed out still read as unknown, which
            // the nest refuses on submit and every view shows neutral.
            FfiReportSubject::Unknown { cbor } => AbuseReportSubject::Unknown(
                decode_strict(&cbor).unwrap_or(CarriedValue(Value::Null)),
            ),
        }
    }
}

impl From<AbuseReportSubject> for FfiReportSubject {
    fn from(s: AbuseReportSubject) -> Self {
        match s {
            AbuseReportSubject::Post { cid } => FfiReportSubject::Post { cid },
            AbuseReportSubject::Message {
                channel,
                record_cid,
            } => FfiReportSubject::Message {
                channel,
                record_cid,
            },
            AbuseReportSubject::Actor { actor_id } => FfiReportSubject::Actor { actor_id },
            AbuseReportSubject::Unknown(v) => FfiReportSubject::Unknown {
                // A decoded value always re-encodes.
                cbor: encode_canonical(&v).map(|b| b.to_vec()).unwrap_or_default(),
            },
        }
    }
}

/// The surface's hold on the subject (`report::ReportTarget`).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiReportTarget {
    pub subject: FfiReportSubject,
    /// Whether the nest holds no readable bytes for it (a message; a gated post).
    pub sealed: bool,
    /// The author's hex actor id — routes the report to their home nest.
    pub author: Option<String>,
    /// The text the client already holds — the excerpt source.
    pub plaintext: Option<String>,
}

impl From<FfiReportTarget> for ReportTarget {
    fn from(t: FfiReportTarget) -> Self {
        ReportTarget {
            subject: t.subject.into(),
            sealed: t.sealed,
            author: t.author,
            plaintext: t.plaintext,
        }
    }
}

impl From<ReportTarget> for FfiReportTarget {
    fn from(t: ReportTarget) -> Self {
        FfiReportTarget {
            subject: t.subject.into(),
            sealed: t.sealed,
            author: t.author,
            plaintext: t.plaintext,
        }
    }
}

/// `ReportTarget::post` — a feed post; `gated` is the sealed rule's post arm
/// (the post was opened through a key).
#[uniffi::export]
pub fn report_post_target(
    cid: String,
    author: String,
    plaintext: String,
    gated: bool,
) -> FfiReportTarget {
    ReportTarget::post(&cid, &author, &plaintext, gated).into()
}

/// `ReportTarget::actor` — the OTHER profile.
#[uniffi::export]
pub fn report_actor_target(actor_id: String) -> FfiReportTarget {
    ReportTarget::actor(&actor_id).into()
}

/// `ReportTarget::message` — a conversation message off its plane ref; `None`
/// for a mail or bridged message, which paints no report verb.
#[uniffi::export]
pub fn report_message_target(
    plane_scope: String,
    record_digest: String,
    sender_actor: Option<String>,
    plaintext: String,
) -> Option<FfiReportTarget> {
    ReportTarget::message(&plane_scope, &record_digest, sender_actor, &plaintext).map(Into::into)
}

/// The sheet's draft (`report::ReportForm`); `reason` is a wire token or
/// `None` until one is picked.
#[derive(uniffi::Record, Clone, Debug, Default, PartialEq, Eq)]
pub struct FfiReportForm {
    pub reason: Option<String>,
    pub note: String,
    pub include_text: bool,
    pub block_author: bool,
}

impl From<FfiReportForm> for ReportForm {
    fn from(f: FfiReportForm) -> Self {
        ReportForm {
            reason: f.reason.as_deref().map(AbuseReportReason::from_token),
            note: f.note,
            include_text: f.include_text,
            block_author: f.block_author,
        }
    }
}

/// One reason in the picker: its wire token and its label.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiReportReasonOption {
    pub reason: String,
    pub label: LocalizedText,
}

/// `report::ReportSheetView` — what `report-sheet` renders.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiReportSheetView {
    pub title: LocalizedText,
    pub reason_label: LocalizedText,
    pub reasons: Vec<FfiReportReasonOption>,
    pub note_label: LocalizedText,
    pub show_include_text: bool,
    pub include_text_label: LocalizedText,
    pub block_author_label: LocalizedText,
    pub submit_label: LocalizedText,
    pub cancel_label: LocalizedText,
    pub can_submit: bool,
    pub blocked_reason: Option<LocalizedText>,
}

impl From<ReportSheetView> for FfiReportSheetView {
    fn from(v: ReportSheetView) -> Self {
        FfiReportSheetView {
            title: v.title,
            reason_label: v.reason_label,
            reasons: v
                .reasons
                .into_iter()
                .map(|o| FfiReportReasonOption {
                    reason: o.reason.token().to_string(),
                    label: o.label,
                })
                .collect(),
            note_label: v.note_label,
            show_include_text: v.show_include_text,
            include_text_label: v.include_text_label,
            block_author_label: v.block_author_label,
            submit_label: v.submit_label,
            cancel_label: v.cancel_label,
            can_submit: v.can_submit,
            blocked_reason: v.blocked_reason,
        }
    }
}

/// A sent report: its id, where it went, and the `report-status` line.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiReportSent {
    pub report_id: String,
    pub routed_to: Vec<String>,
    pub acknowledgement: LocalizedText,
}

/// One `moderation-report-item` row: the entry plus its shared wording.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiReportLedgerRow {
    pub report_id: String,
    pub subject: FfiReportSubject,
    /// Microsecond epoch.
    pub created_at: i64,
    pub reason: LocalizedText,
    pub status: LocalizedText,
    pub outcome: Option<LocalizedText>,
    pub routed_to: LocalizedText,
    pub can_withdraw: bool,
}

fn ledger_row(entry: AbuseReportMineEntry) -> FfiReportLedgerRow {
    let LedgerRowView {
        reason,
        status,
        outcome,
        routed_to,
        can_withdraw,
    } = report::ledger_row_view(&entry);
    FfiReportLedgerRow {
        report_id: entry.report_id,
        subject: entry.subject.into(),
        created_at: entry.created_at,
        reason,
        status,
        outcome,
        routed_to,
        can_withdraw,
    }
}

/// One `admin-nest-report-item` row. `origin` names a local reporter by handle
/// or a forwarded report as "a user of <nest>" — never a forwarded reporter.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiReportQueueRow {
    pub report_id: String,
    pub subject: FfiReportSubject,
    pub subject_actor: Option<String>,
    pub reason: LocalizedText,
    pub note: Option<String>,
    pub excerpt: Option<String>,
    pub origin: LocalizedText,
    /// Microsecond epoch.
    pub created_at: i64,
    /// Whether `admin-nest-report-open-takedown-button` renders — posts and
    /// messages only (`report::takedown_prefill`).
    pub can_open_takedown: bool,
}

fn queue_row(entry: AbuseReportQueueEntry) -> FfiReportQueueRow {
    let view = report::queue_row_view(&entry);
    FfiReportQueueRow {
        report_id: entry.report_id,
        subject: entry.subject.into(),
        subject_actor: entry.subject_actor,
        reason: view.reason,
        note: view.note,
        excerpt: view.excerpt,
        origin: view.origin,
        created_at: entry.created_at,
        can_open_takedown: view.can_open_takedown,
    }
}

/// What *open takedown* pre-fills the legal-takedown console with.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiTakedownPrefill {
    pub content_id: String,
    /// `true` selects the conversation kind, else a post.
    pub conversation: bool,
}

/// `report::takedown_prefill` — `None` for an account or an unknown subject.
#[uniffi::export]
pub fn report_takedown_prefill(subject: FfiReportSubject) -> Option<FfiTakedownPrefill> {
    use fauna_client_moderation::takedown::TakedownContentType;
    report::takedown_prefill(&subject.into()).map(|form| FfiTakedownPrefill {
        content_id: form.content_id,
        conversation: form.content_type == TakedownContentType::Conversation,
    })
}

/// `report::message_subject` — the report subject for a conversation message
/// from its plane ref (`plane_scope`, `record_digest`); `None` for a mail or
/// bridged message, which paints no report verb.
#[uniffi::export]
pub fn report_message_subject(
    plane_scope: String,
    record_digest: String,
) -> Option<FfiReportSubject> {
    report::message_subject(&plane_scope, &record_digest).map(Into::into)
}

/// The sheet's per-keystroke fold.
#[uniffi::export]
pub fn report_sheet_view(target: FfiReportTarget, form: FfiReportForm) -> FfiReportSheetView {
    let target: ReportTarget = target.into();
    report::report_sheet_view(&target.subject, target.sealed, &form.into()).into()
}

/// The line a failed send paints on the page's `error-message`.
#[uniffi::export]
pub fn report_failed(error: String) -> LocalizedText {
    report::report_failed(error)
}

/// The ledger's header and empty-state words.
#[uniffi::export]
pub fn report_ledger_title() -> LocalizedText {
    report::ledger_title()
}

#[uniffi::export]
pub fn report_ledger_empty() -> LocalizedText {
    report::ledger_empty()
}

/// The line a withdraw paints — `None` on success.
#[uniffi::export]
pub fn report_withdraw_verdict(error: Option<String>) -> LocalizedText {
    report::withdraw_verdict(error)
}

/// The line the admin queue paints after resolving a row; `acted` false is a
/// dismissal.
#[uniffi::export]
pub fn report_resolve_verdict(acted: bool, error: Option<String>) -> LocalizedText {
    report::resolve_verdict(outcome_of(acted), error)
}

fn outcome_of(acted: bool) -> AbuseReportOutcome {
    if acted {
        AbuseReportOutcome::Acted
    } else {
        AbuseReportOutcome::Dismissed
    }
}

#[fauna_uniffi_async::export]
impl FfiModerationClient {
    /// `fauna.moderation.abuse_report.submit`, built from the sheet by the
    /// shared `report::report_request` (bounds, excerpt rule). A sheet the view
    /// would not let send is refused before anything leaves. `block_author` is
    /// recorded only — the app chains `fauna.knocks.block` itself, and hides
    /// the subject with `hide_reported`.
    pub async fn abuse_report_submit(
        &self,
        target: FfiReportTarget,
        form: FfiReportForm,
    ) -> Result<FfiReportSent, FfiError> {
        let request = report::report_request(&target.into(), &form.into())
            .ok_or_else(|| general_err("the report cannot be sent yet"))?;
        let reply = self
            .client()
            .abuse_report_submit(request)
            .await
            .map_err(stringify)?;
        Ok(FfiReportSent {
            acknowledgement: report::report_acknowledgement(&reply.routed_to),
            report_id: reply.report_id,
            routed_to: reply.routed_to,
        })
    }

    /// `fauna.moderation.abuse_report.mine` — the ledger, newest first, worded.
    pub async fn abuse_report_mine(&self) -> Result<Vec<FfiReportLedgerRow>, FfiError> {
        let reply = self.client().abuse_report_mine().await.map_err(stringify)?;
        Ok(reply.reports.into_iter().map(ledger_row).collect())
    }

    /// `fauna.moderation.abuse_report.withdraw` — errors surface for
    /// [`report_withdraw_verdict`] to word.
    pub async fn abuse_report_withdraw(&self, report_id: String) -> Result<(), FfiError> {
        self.client()
            .abuse_report_withdraw(report_id)
            .await
            .map_err(stringify)?;
        Ok(())
    }
}

#[fauna_uniffi_async::export]
impl FfiAdminClient {
    /// `fauna.moderation.abuse_report.queue` — the open reports, local and
    /// forwarded, oldest first, worded. Admin-class.
    pub async fn abuse_report_queue(&self) -> Result<Vec<FfiReportQueueRow>, FfiError> {
        let reply = self
            .client()
            .abuse_report_queue()
            .await
            .map_err(stringify)?;
        Ok(reply.reports.into_iter().map(queue_row).collect())
    }

    /// `fauna.moderation.abuse_report.resolve` — a record, not an action;
    /// `acted` false dismisses. Admin-class.
    pub async fn abuse_report_resolve(
        &self,
        report_id: String,
        acted: bool,
    ) -> Result<(), FfiError> {
        self.client()
            .abuse_report_resolve(report_id, outcome_of(acted))
            .await
            .map_err(stringify)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message() -> FfiReportTarget {
        FfiReportTarget {
            subject: FfiReportSubject::Message {
                channel: "c".into(),
                record_cid: "r".into(),
            },
            sealed: true,
            author: Some("ab".repeat(32)),
            plaintext: Some("the words".into()),
        }
    }

    /// The face is the shared view, token for token: all eight reasons, the
    /// excerpt checkbox for a sealed subject, and no send until a reason is
    /// picked.
    #[test]
    fn the_sheet_face_is_the_shared_view() {
        let empty = report_sheet_view(message(), FfiReportForm::default());
        assert_eq!(empty.reasons.len(), 8);
        assert_eq!(empty.reasons[0].reason, "spam");
        assert!(empty.show_include_text);
        assert!(!empty.can_submit);
        let picked = report_sheet_view(
            message(),
            FfiReportForm {
                reason: Some("harassment".into()),
                ..Default::default()
            },
        );
        assert!(picked.can_submit && picked.blocked_reason.is_none());
    }

    #[test]
    fn a_subject_crosses_the_face_unchanged() {
        let subject = FfiReportSubject::Actor {
            actor_id: "cd".repeat(32),
        };
        let wire: AbuseReportSubject = subject.clone().into();
        assert_eq!(FfiReportSubject::from(wire), subject);
    }
}
