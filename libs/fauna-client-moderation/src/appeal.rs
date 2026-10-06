//! The moderation queue's **appeal** decisions (`moderation.md` § Legal
//! takedown — the transparency triple's second leg), derived once here rather
//! than seven times (priority #2).
//!
//! Three decisions live here because each is one an app would get wrong alone:
//!
//! 1. **Which rows may be appealed at all.** The queue is a *union* of two
//!    unlike things (`detections::merge_queue`): nest-issued obligation rows,
//!    which record a decision someone made, and the client's own post-decrypt
//!    local detections, which record nothing but this device's classifier
//!    guessing. Only the first is appealable — there is no nest-side record
//!    behind a local detection, so an appeal against one is refused by the
//!    nest's own gate and would be meaningless if it were not. An app deriving
//!    this from `source` and another from `action` is exactly the drift the
//!    shared layer exists to stop.
//! 2. **An appeal always carries a reason, and a bounded one.** The wire refuses
//!    an empty `reason`, and one over `MAX_APPEAL_REASON_BYTES`
//!    (`invalid_params` both); rendering those guards client-side means the
//!    submit control is disabled *with a stated reason* instead of a dispatch
//!    that bounces.
//! 3. **The verdict wording.** A recorded appeal is not `error-message`
//!    material, and the sentence has to say what actually happened — the appeal
//!    is *recorded for review*, not granted. Every app says the same thing or
//!    the transparency framing drifts.

use crate::detections::{QueueRow, QueueRowSource};
use fauna_core::localized::LocalizedText;
use fauna_protocol::moderation::MAX_APPEAL_REASON_BYTES;
use serde::{Deserialize, Serialize};

/// Whether this queue row carries a decision there is something to appeal
/// against (decision 1).
///
/// True exactly for a **server** row: one the nest issued from an
/// `obligation_action_records` entry, which is the enforcement record the
/// nest's appeal gate looks for. A local detection carries no `action` and no
/// nest-side record — its lever is the train-correction button, not an appeal.
pub fn row_is_appealable(row: &QueueRow) -> bool {
    matches!(row.source, QueueRowSource::Server)
}

/// The appeal draft, exactly as typed — apps pass their raw field values and
/// render whatever comes back; nothing here is trimmed in place.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppealForm {
    /// The content id under appeal — the queue row's own `content_id`.
    pub content_id: String,
    /// Why the decision should be reviewed.
    pub reason: String,
}

/// What the appeal surface renders, derived once per keystroke from the form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppealFormView {
    /// Whether the submit control may be offered.
    pub can_submit: bool,
    /// Why not, when it may not — the surface states the reason instead of
    /// painting a dead control.
    pub blocked_reason: Option<LocalizedText>,
    /// The submit control's label.
    pub submit_label: LocalizedText,
    /// The decision surface: names the content the appeal will be filed
    /// against, so an appeal is never submitted blind against the wrong row.
    pub summary: LocalizedText,
}

/// Fold the form into the appeal surface's render decisions.
pub fn appeal_form_view(form: &AppealForm) -> AppealFormView {
    let content_id = form.content_id.trim();
    let reason = form.reason.trim();

    let blocked_reason = if content_id.is_empty() {
        Some(LocalizedText::key("moderation.appeal_blocked_no_content"))
    } else if reason.is_empty() {
        // The wire's own guard, rendered (decision 2).
        Some(LocalizedText::key("moderation.appeal_blocked_no_reason"))
    } else if reason.len() > MAX_APPEAL_REASON_BYTES {
        // The wire's length bound, rendered — the same constant the nest
        // refuses on (decision 2).
        Some(LocalizedText::key(
            "moderation.appeal_blocked_reason_too_long",
        ))
    } else {
        None
    };

    AppealFormView {
        can_submit: blocked_reason.is_none(),
        blocked_reason,
        submit_label: LocalizedText::key("moderation.appeal_submit"),
        summary: LocalizedText::key_arg("moderation.appeal_summary", "content_id", content_id),
    }
}

/// The outcome line the appeal surface paints after a dispatch — deliberately
/// not `error-message` (a recorded appeal is the common case). The success
/// wording says the appeal was **recorded for review**, never that it was
/// granted: the nest logs it to the audit trail for an admin to act on
/// (`moderation.md` § Legal takedown). `error` is the transport/handler error
/// rendered by the caller's `Display`, when the dispatch failed — including the
/// `fauna.moderation.not_found` an appeal against un-actioned content earns.
pub fn appeal_verdict(error: Option<String>) -> LocalizedText {
    match error {
        Some(e) => LocalizedText::key_arg("moderation.appeal_failed", "error", e),
        None => LocalizedText::key("moderation.appeal_recorded"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(source: QueueRowSource, action: Option<u8>) -> QueueRow {
        QueueRow {
            content_id: "ab".repeat(32),
            content_type: "post".into(),
            category: "illegal".into(),
            confidence_per_mille: 1000,
            action,
            timestamp: 1,
            source,
        }
    }

    /// Decision 1: a nest-issued enforcement row is appealable; a local
    /// detection is not — there is no decision behind it to appeal.
    #[test]
    fn only_a_server_row_offers_an_appeal() {
        assert!(row_is_appealable(&row(QueueRowSource::Server, Some(7))));
        assert!(!row_is_appealable(&row(QueueRowSource::Local, None)));
    }

    /// Decision 2: a reasonless appeal is never submittable, and the reason
    /// names the missing reason (not the missing id).
    #[test]
    fn a_reasonless_appeal_is_blocked_with_the_reason_reason() {
        let view = appeal_form_view(&AppealForm {
            content_id: "ab".repeat(32),
            reason: "   ".into(),
        });
        assert!(!view.can_submit);
        assert_eq!(
            view.blocked_reason.expect("blocked").key,
            "moderation.appeal_blocked_no_reason"
        );
    }

    /// The reason is bounded by the same constant the nest refuses on, so an
    /// over-long draft is blocked here instead of bouncing off the wire — and a
    /// reason exactly at the bound still submits. Measured on the trimmed text,
    /// which is what the apps send.
    #[test]
    fn an_over_long_reason_is_blocked_at_the_wire_bound() {
        let at_bound = AppealForm {
            content_id: "ab".repeat(32),
            reason: format!("  {}  ", "x".repeat(MAX_APPEAL_REASON_BYTES)),
        };
        assert!(appeal_form_view(&at_bound).can_submit);

        let view = appeal_form_view(&AppealForm {
            content_id: "ab".repeat(32),
            reason: "x".repeat(MAX_APPEAL_REASON_BYTES + 1),
        });
        assert!(!view.can_submit);
        assert_eq!(
            view.blocked_reason.expect("blocked").key,
            "moderation.appeal_blocked_reason_too_long"
        );
    }

    /// An empty content id blocks first — there is nothing to appeal against.
    #[test]
    fn an_empty_content_id_blocks_before_the_reason() {
        let view = appeal_form_view(&AppealForm {
            content_id: "  ".into(),
            reason: "I hold the licence for this recording".into(),
        });
        assert!(!view.can_submit);
        assert_eq!(
            view.blocked_reason.expect("blocked").key,
            "moderation.appeal_blocked_no_content"
        );
    }

    /// A complete draft submits, and the summary names the content as it will
    /// be sent (trimmed).
    #[test]
    fn a_complete_appeal_submits_and_names_its_content() {
        let id = "cd".repeat(32);
        let view = appeal_form_view(&AppealForm {
            content_id: format!("  {id} "),
            reason: " I hold the licence ".into(),
        });
        assert!(view.can_submit);
        assert!(view.blocked_reason.is_none());
        assert_eq!(view.summary.key, "moderation.appeal_summary");
        assert_eq!(view.summary.args["content_id"], id);
    }

    /// Decision 3: success says *recorded*, never *granted*; a failure carries
    /// the error as a named arg.
    #[test]
    fn the_verdict_words_each_outcome() {
        assert_eq!(appeal_verdict(None).key, "moderation.appeal_recorded");
        let failed = appeal_verdict(Some("not_found".into()));
        assert_eq!(failed.key, "moderation.appeal_failed");
        assert_eq!(failed.args["error"], "not_found");
    }
}
